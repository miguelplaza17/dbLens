// Screenshots of the schema board with the mock backend (src/mock.ts).
// Run the dev server first (`npx vite --port 1420`), then:
//   node docs/shots/schema.cjs [baseURL]
// Writes docs/shots/schema-<scene>-dark.png and -light.png, and prints how
// long a frame takes while panning and zooming a 200-table board.

const path = require("path");
const { chromium } = require("playwright");

const base = process.argv[2] || "http://localhost:1420/";
const out = __dirname;

async function open(page, extra = "") {
  await page.locator(".sidebar-row", { hasText: "demo @ localhost" }).first().click();
  await page.locator(".sidebar-row", { hasText: "customers" }).waitFor();
  await page.keyboard.press("Control+3");
  await page.locator(".schema-canvas canvas").first().waitFor();
  await page.waitForTimeout(300);
}

async function scene(page, name) {
  const canvas = page.locator(".schema-canvas canvas").first();
  const box = async () => canvas.boundingBox();
  switch (name) {
    case "board":
      await open(page);
      break;
    case "zoomed": {
      await open(page);
      await page.keyboard.press("Control+=");
      await page.waitForTimeout(150);
      await page.keyboard.press("Control+=");
      await page.waitForTimeout(200);
      // Hover a card so its links light up.
      const b = await box();
      await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
      await page.waitForTimeout(200);
      break;
    }
    case "folded":
      await open(page);
      await page.locator(".schema-controls button", { hasText: /Key Columns Only|Só Colunas-Chave/ }).click();
      await page.waitForTimeout(200);
      break;
    case "big":
      await open(page);
      break;
    case "big-close": {
      await open(page);
      const b = await box();
      // Presses closer than 120 ms count once (native/actions.ts), so wait between them.
      for (let i = 0; i < 6; i++) {
        await page.keyboard.press("Control+=");
        await page.waitForTimeout(150);
      }
      await page.mouse.move(b.x + b.width / 3, b.y + b.height / 3);
      await page.waitForTimeout(200);
      break;
    }
  }
}

async function measure(page, label) {
  // Pan and zoom the big board for a second and time the frames.
  const canvas = page.locator(".schema-canvas canvas").first();
  const b = await canvas.boundingBox();
  await page.evaluate(() => {
    window.__frames = [];
    let last = performance.now();
    const tick = (now) => {
      window.__frames.push(now - last);
      last = now;
      if (window.__frames.length < 400) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  await page.mouse.move(b.x + 300, b.y + 300);
  await page.mouse.down();
  for (let i = 0; i < 60; i++) await page.mouse.move(b.x + 300 + i * 6, b.y + 300 + i * 3);
  await page.mouse.up();
  for (let i = 0; i < 30; i++) await page.mouse.wheel(0, i < 15 ? -60 : 60);
  const frames = await page.evaluate(() => window.__frames.slice(2));
  frames.sort((a, b) => a - b);
  const p = (q) => frames[Math.min(frames.length - 1, Math.floor(frames.length * q))].toFixed(1);
  console.log(`200 tables, ${label}: ${frames.length} frames, median ${p(0.5)} ms, p95 ${p(0.95)} ms, max ${p(1)} ms`);
}

(async () => {
  const browser = await chromium.launch();
  const scenes = (process.env.SHOTS || "board,zoomed,folded,big,big-close").split(",");
  for (const theme of ["dark", "light"]) {
    for (const name of scenes) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, locale: "en-US" });
      const page = await context.newPage();
      page.on("pageerror", (e) => console.error(`[${name}] ${e.message}`));
      page.on("console", (m) => m.type() === "error" && console.error(`[${name}] ${m.text()}`));
      const big = name.startsWith("big") ? "&schemaTables=200" : "";
      await page.goto(`${base}?theme=${theme}&lang=${process.env.SHOT_LANG || "en"}${big}`);
      await page.locator(".sidebar").waitFor();
      await page.waitForTimeout(200);
      await scene(page, name);
      const file = path.join(out, `schema-${name}-${theme}.png`);
      await page.screenshot({ path: file });
      console.log(file);
      if (name.startsWith("big") && theme === "dark") await measure(page, name === "big" ? "fitted" : "zoomed in");
      await context.close();
    }
  }
  await browser.close();
})();
