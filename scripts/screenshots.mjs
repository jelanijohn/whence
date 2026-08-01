// Marketing screenshots, headlessly. Boots the frontend in mock mode
// (VITE_WHENCE_MOCK=1 — see src/lib/tauri.mock.ts for the posed scenario) and
// captures the widget states with Playwright: transparent PNGs at 2x, panel
// shadow included, into docs/screenshots/.
//
// One-time setup:  pnpm exec playwright install chromium
//                  (WSL2, if launch fails: sudo pnpm exec playwright install-deps chromium)
// Run:             pnpm screenshots
import { spawn } from "node:child_process";
import { mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

// 1427 so a running `pnpm dev` (1425, strictPort) or `tauri:dev2` (1435) never collides.
const PORT = 1427;
const BASE_URL = `http://localhost:${PORT}`;
// fileURLToPath, not .pathname — the latter mangles Windows paths (`/C:/...`).
const OUT_DIR = fileURLToPath(new URL("../docs/screenshots/", import.meta.url));

// Widget geometry — mirrors the height math in src/routes/+page.svelte
// (TITLE_H + rows*ROW_H + expandedSources*SOURCE_ROW_H + context*CONTEXT_ROW_H
// + LIST_PAD) for the mock scenario: 3 rows, 1 context line, 2 whoami sources.
const WIDTH = 300;
const ROSTER_H = 44 + 3 * 30 + 1 * 16 + 12; // 162
const EXPANDED_H = ROSTER_H + 2 * 24; // 210
const TIMELINE_H = ROSTER_H + 168; // 330
// Settings window size — src-tauri/src/commands.rs `open_settings`.
const SETTINGS_W = 400;
const SETTINGS_H = 560;
// Breathing room so the panel's box-shadow isn't clipped at the PNG edge.
const PAD = 24;
// Pinned page-context time (clock.setFixedTime): Date.now() is fixed — so the
// mock's offsets and every timer readout are identical across themes and runs —
// while real timers keep firing (the settings "Saved" flash still clears). An
// afternoon, so the mock's 09:04–15:47 workday is fully in the past.
const FROZEN_TIME = new Date("2026-07-20T16:20:00");

function startDevServer() {
  return spawn("pnpm", ["exec", "vite", "dev"], {
    env: { ...process.env, VITE_WHENCE_MOCK: "1", WHENCE_DEV_PORT: String(PORT) },
    stdio: "ignore",
  });
}

async function waitForServer(timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(BASE_URL);
      if (res.ok) return;
    } catch {
      // Not up yet.
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error(`vite dev never answered on ${BASE_URL}`);
}

/** Icon/text fonts are lazy; force them so buttons render glyphs, not ligature text. */
async function settleFonts(page) {
  await page.evaluate(() =>
    Promise.all([
      document.fonts.load('18px "Material Symbols Rounded"'),
      document.fonts.load("14px Inter"),
      document.fonts.ready,
    ]),
  );
  await page.waitForTimeout(250);
}

// Dark variant: flip the page's real "Dark mode" switch when it has one (its
// live-preview drives the `dark` class, and the knob renders ON — keeping the
// shot self-consistent); pages without a switch get the class injected directly.
async function captureBothThemes(page, name, goDark = null) {
  // animations: "disabled" pins the status dot's infinite tw-pulse to a fixed
  // phase — with the frozen clock, that makes captures byte-identical across runs.
  const opts = { omitBackground: true, animations: "disabled" };
  await page.screenshot({ ...opts, path: `${OUT_DIR}${name}-light.png` });
  if (goDark) await goDark();
  else await page.evaluate(() => document.documentElement.classList.add("dark"));
  await page.waitForTimeout(100);
  await page.screenshot({ ...opts, path: `${OUT_DIR}${name}-dark.png` });
}

/** A widget page: padded viewport (shadow room) with the panel inset to fit. */
async function widgetPage(browser, height) {
  const page = await browser.newPage({
    viewport: { width: WIDTH + 2 * PAD, height: height + 2 * PAD },
    deviceScaleFactor: 2,
  });
  await page.clock.setFixedTime(FROZEN_TIME);
  await page.goto(BASE_URL);
  await page.addStyleTag({
    content: `body { padding: ${PAD}px; } .panel { height: calc(100vh - ${2 * PAD}px); }`,
  });
  await page.locator('[title="whoami"]').waitFor();
  await settleFonts(page);
  return page;
}

async function main() {
  await mkdir(OUT_DIR, { recursive: true });
  const server = startDevServer();
  let browser;
  try {
    await waitForServer();
    browser = await chromium.launch();

    // Compact roster.
    let page = await widgetPage(browser, ROSTER_H);
    await captureBothThemes(page, "roster");
    await page.close();

    // Roster with the focused project's sources expanded.
    page = await widgetPage(browser, EXPANDED_H);
    await page.locator('[title="whoami"]').getByTitle("Show sources").click();
    await page.waitForTimeout(100);
    await captureBothThemes(page, "roster-expanded");
    await page.close();

    // Timeline view.
    page = await widgetPage(browser, TIMELINE_H);
    await page.getByTitle("Today's blocks").click();
    await page.getByText("blocks today").waitFor();
    await captureBothThemes(page, "timeline");
    await page.close();

    // Settings window — decorated, paints its own opaque backdrop, so no pad.
    page = await browser.newPage({
      viewport: { width: SETTINGS_W, height: SETTINGS_H },
      deviceScaleFactor: 2,
    });
    await page.clock.setFixedTime(FROZEN_TIME);
    await page.goto(`${BASE_URL}/settings`);
    await page.getByText("Receiver auth").waitFor(); // form + auth section hydrated
    await settleFonts(page);
    await captureBothThemes(page, "settings", async () => {
      await page.getByRole("switch", { name: "Dark mode" }).click();
      // Save and wait out the "Saved" flash (1.8s) so the footer reads settled,
      // not mid-edit — the mock echoes the settings back, so Save just works.
      await page.getByRole("button", { name: "Save" }).click();
      await page.getByText("Saved", { exact: true }).waitFor();
      await page.getByText("Saved", { exact: true }).waitFor({ state: "hidden" });
    });
    await page.close();

    console.log(`Wrote 8 screenshots to ${OUT_DIR}`);
  } finally {
    await browser?.close();
    server.kill();
  }
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
