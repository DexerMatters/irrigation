// End-to-end: edit a source View in place and watch the change flow.
//
//   OBSERVATORY_URL=http://127.0.0.1:7791 node scripts/canvas-smoke.mjs
//
// The card and the text come from the host, so the same script runs against any
// board:
//
//   SMOKE_VIEW=SourceFile SMOKE_TEXT='{"a":1}' OBSERVATORY_URL=… node scripts/canvas-smoke.mjs
//
// Asserts that (1) the editor opens on double-click, (2) a keystroke commits a
// new revision without pressing anything, (3) the board marks what moved, and
// (4) the console stays clean.

import { chromium } from "playwright-core";

const base = process.env.OBSERVATORY_URL ?? "http://127.0.0.1:7777";
const view = process.env.SMOKE_VIEW ?? "Syntax";
// The default text carries a stamp, so running the script twice in a row is two
// edits and not a no-op.
const stamp = String(Date.now() % 100_000);
// `{{stamp}}` is replaced, so a caller can write one template and still get a
// fresh edit on every run.
const text = (process.env.SMOKE_TEXT ?? `x := {{stamp}}\ny := 2\nprint(show(x), 3)`).replaceAll(
  "{{stamp}}",
  stamp,
);
const browser = await chromium.launch({
  executablePath: `${process.env.HOME}/.cache/ms-playwright/chromium_headless_shell-1248/chrome-headless-shell-linux64/chrome-headless-shell`,
  args: ["--no-sandbox"],
});
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
const errors = [];
page.on("console", (message) => {
  if (message.type() === "error") errors.push(message.text());
});
page.on("pageerror", (error) => errors.push(String(error)));

await page.goto(base, { waitUntil: "domcontentloaded" });
await page.waitForSelector(".card", { timeout: 20000 });
await page.waitForTimeout(900);

const before = await page.evaluate(async () => {
  const data = await fetch("/api/v1/views").then((response) => response.json());
  const revisions = await fetch("/api/v1/revisions").then((response) => response.json());
  return {
    revision: Math.max(0, ...revisions.map((record) => record.revision)),
    views: data.map((v) => `${v.name.split("::").pop()}:${v.state}`),
  };
});

const card = page.locator(".card", { has: page.locator(".card-name", { hasText: view }) }).first();
await card.dblclick();
await page.waitForSelector(".editor-area", { timeout: 10000 });
await page.fill(".editor-area", text);
// No submit: the edit commits by itself.
await page.waitForTimeout(2600);

const after = await page.evaluate(async () => {
  const data = await fetch("/api/v1/views").then((response) => response.json());
  const revisions = await fetch("/api/v1/revisions").then((response) => response.json());
  return {
    revision: Math.max(0, ...revisions.map((record) => record.revision)),
    views: data.map((v) => `${v.name.split("::").pop()}:${v.state}`),
  };
});

const board = await page.evaluate(() => ({
  blink: document.querySelectorAll("[data-blink='1']").length,
  changed: [...document.querySelectorAll(".card-changed .card-name")].map((node) => node.textContent),
  notes: [...document.querySelectorAll(".changed-note, .same-note")].map((node) => node.textContent),
  markedRows: document.querySelectorAll(".row-changed").length,
  ran: [...document.querySelectorAll(".component-changed .component-name")].map(
    (node) => node.textContent,
  ),
}));
const reply = await card.locator(".editor-reply").textContent().catch(() => "");

const advanced = after.revision > before.revision;
const marked = board.changed.length > 0;
console.log(
  JSON.stringify(
    { before, after, advanced, marked, board, reply, errors },
    null,
    2,
  ),
);
await page.screenshot({ path: process.env.SMOKE_SHOT ?? "canvas-smoke.png" });
await browser.close();
process.exit(errors.length === 0 && advanced && marked ? 0 : 1);
