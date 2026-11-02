// Screenshots of the UI with the mock backend (src/mock.ts), in a plain browser.
// Run the dev server first (`npx vite --port 1420`), then:
//   node docs/shots/take.cjs [baseURL]
// Writes docs/shots/<name>-dark.png and -light.png.

const path = require("path");
const { chromium } = require("playwright");

const base = process.argv[2] || "http://localhost:1420/";
const out = __dirname;

async function scene(page, name) {
  const pause = (ms) => page.waitForTimeout(ms);
  const sidebarRow = (text) => page.locator(".sidebar-row", { hasText: text }).first();
  switch (name) {
    case "empty":
      break;
    case "connect-sheet":
      await page.keyboard.press("Control+N");
      await page.locator(".sheet input.mono").fill("postgres://demo:demo@localhost:5432/demo?sslmode=require");
      await pause(300);
      break;
    case "connect-ssh":
      await page.keyboard.press("Control+N");
      await page.locator(".sheet input.mono").fill("mysql://shop@10.0.0.12:3306/shop");
      await page.locator(".sheet .switch").check();
      await page.locator(".sheet .form-row", { hasText: /Server|Servidor/ }).locator("input").fill("deploy@bastion.example.com");
      await pause(300);
      break;
    case "table":
      await sidebarRow("demo @ localhost").click();
      await page.locator(".sidebar-row", { hasText: "customers" }).waitFor();
      await sidebarRow("customers").click();
      await page.locator(".grid-row").first().waitFor();
      await page.locator(".grid-row").nth(2).click();
      await pause(300);
      break;
    case "sql":
      await sidebarRow("demo @ localhost").click();
      await page.locator(".sidebar-row", { hasText: "orders" }).waitFor();
      await page.locator(".cm-content").click();
      await page.keyboard.type("select o.status, count(*) as orders, sum(o.total) as revenue\nfrom sales.orders o\ngroup by o.status\norder by revenue desc;");
      await page.keyboard.press("Control+Enter");
      await page.locator(".grid-row").first().waitFor();
      await pause(300);
      break;
    case "row-editor":
      await sidebarRow("demo @ localhost").click();
      await page.locator(".sidebar-row", { hasText: "customers" }).waitFor();
      await sidebarRow("customers").click();
      await page.locator(".grid-row").first().waitFor();
      await page.locator(".grid-row").nth(1).locator(".grid-number").dblclick();
      await pause(300);
      break;
    case "redis":
      await sidebarRow("cache @ localhost").click();
      await page.locator(".sidebar-row", { hasText: "user:1" }).waitFor();
      await sidebarRow("user:1").click();
      await page.locator(".grid-row").first().waitFor();
      await pause(300);
      break;
  }
}

(async () => {
  const browser = await chromium.launch();
  const scenes = (process.env.SHOTS || "empty,connect-sheet,connect-ssh,table,sql,row-editor,redis").split(",");
  for (const theme of ["dark", "light"]) {
    for (const name of scenes) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, locale: "en-US" });
      const page = await context.newPage();
      page.on("pageerror", (e) => console.error(`[${name}] ${e.message}`));
      await page.goto(`${base}?theme=${theme}&lang=${process.env.SHOT_LANG || "en"}`);
      await page.locator(".sidebar").waitFor();
      await page.waitForTimeout(200);
      await scene(page, name);
      const file = path.join(out, `${name}-${theme}.png`);
      await page.screenshot({ path: file });
      console.log(file);
      await context.close();
    }
  }
  await browser.close();
})();
