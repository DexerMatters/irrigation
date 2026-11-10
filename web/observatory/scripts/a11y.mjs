// Accessibility pass over the live board.
//
//   OBSERVATORY_URL=http://127.0.0.1:7777 node scripts/a11y.mjs
//
// Runs axe-core in the same Chromium the smoke test uses and fails on serious or
// critical violations. It also walks the keyboard path: Tab into the bar, focus
// the first plate's Edit button, open the editor with Enter, and close it with
// Escape.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { chromium } from "playwright-core";

const require = createRequire(import.meta.url);
const axeSource = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");

const base = process.env.OBSERVATORY_URL ?? "http://127.0.0.1:7777";
const browser = await chromium.launch({
  executablePath: `${process.env.HOME}/.cache/ms-playwright/chromium_headless_shell-1248/chrome-headless-shell-linux64/chrome-headless-shell`,
  args: ["--no-sandbox"],
});
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
await page.goto(base, { waitUntil: "domcontentloaded" });
await page.waitForSelector(".card", { timeout: 20000 });
await page.waitForTimeout(800);

const keyboard = [];
await page.keyboard.press("Tab");
keyboard.push(await page.evaluate(() => document.activeElement?.className ?? "none"));
// Open the first editable card's editor from the keyboard. The fold control is
// also a `.card-action`, so select the one that carries the pressed state.
const edit = page.locator(".card-action[aria-pressed]").first();
await edit.focus();
keyboard.push(await page.evaluate(() => document.activeElement?.textContent ?? "none"));
await page.keyboard.press("Enter");
await page.waitForTimeout(300);
const editorOpen = await page.locator(".editor-area").count();
await page.keyboard.press("Escape");
await page.waitForTimeout(200);

const results = {};
await page.addScriptTag({ content: axeSource });
results["canvas"] = await page.evaluate(async () => {
  const outcome = await window.axe.run(document, {
    runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"] },
  });
  return outcome.violations.map((violation) => ({
    id: violation.id,
    impact: violation.impact,
    nodes: violation.nodes.length,
    help: violation.help,
    sample: violation.nodes[0]?.target?.join(" "),
  }));
});

const serious = Object.entries(results).flatMap(([mode, violations]) =>
  violations
    .filter((violation) => violation.impact === "serious" || violation.impact === "critical")
    .map((violation) => ({ mode, ...violation })),
);

console.log(
  JSON.stringify(
    { keyboard, editorOpen, violations: results, serious },
    null,
    2,
  ),
);
await browser.close();
process.exit(serious.length === 0 ? 0 : 1);
