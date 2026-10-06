import { test, expect } from "@playwright/test";
const directory = "../docs/desktop/screenshots";
test("render requested preview views with explicit simulated label", async ({
  page,
}) => {
  const views = [
    ["library-dark", "?theme=dark"],
    ["library-light", "?theme=light"],
    ["empty-library", "?empty=1&theme=light"],
    ["add-game", "?view=add&theme=light"],
    ["analysis-results", "?view=analysis&theme=light"],
    ["optimization-progress", "?view=optimization&theme=dark"],
    ["settings-diagnostics", "?view=settings&theme=light"],
  ];
  for (const [name, query] of views) {
    await page.goto(`/${query}`);
    await expect(page.getByText("Preview Mode · simulated data")).toBeVisible();
    if (name === "settings-diagnostics") {
      await page.getByRole("button", { name: "Run diagnostics" }).click();
      await expect(
        page.locator("p").filter({ hasText: "Open the desktop application" }),
      ).toBeVisible();
    }
    await page.screenshot({ path: `${directory}/${name}.png`, fullPage: true });
  }
});
test("preview add, analyze, optimize, verify, navigation and theme are interactive", async ({
  page,
}) => {
  await page.goto("/?empty=1&theme=light");
  await page.getByRole("button", { name: "Add your first game" }).click();
  await page.getByRole("button", { name: "Add to Library" }).click();
  await page.getByRole("button", { name: "Analyze Game", exact: true }).click();
  await page.getByRole("button", { name: "Review & Optimize" }).click();
  await expect(page.getByText("Measured analysis")).toBeVisible();
  await page
    .getByRole("button", { name: "Optimize Game", exact: true })
    .click();
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(
    page.getByRole("button", { name: "Verify Store", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Verify Store", exact: true }).click();
  await page.getByRole("button", { name: "Close dialog" }).click();
  await expect(
    page.getByRole("button", { name: "Mount Store", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Mount Store", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText(
    "require the native PlaySparse app",
  );
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("button", { name: "dark", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});
test("layouts stay inside viewport at requested sizes", async ({ page }) => {
  for (const [width, height] of [
    [1024, 700],
    [1280, 800],
    [1440, 900],
    [680, 520],
  ]) {
    await page.setViewportSize({ width, height });
    await page.goto("/?theme=light");
    await expect(
      page.getByRole("heading", { name: "Your games, lighter." }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBeTruthy();
  }
});

test("readiness, inspection, literal candidate confirmation and recovery are reachable", async ({
  page,
}) => {
  await page.goto("/?empty=1&theme=light");
  await expect(
    page.getByRole("region", { name: "System readiness" }),
  ).toContainText("Unknown");
  await page.getByRole("button", { name: "Run diagnostics again" }).click();
  await page.getByRole("button", { name: "Add your first game" }).click();
  await expect(page.getByText("Detected title")).toBeVisible();
  await page.getByRole("button", { name: "Add to Library" }).click();
  await page.getByRole("button", { name: "Analyze Game", exact: true }).click();
  await page.getByRole("button", { name: "Review & Optimize" }).click();
  await page
    .getByRole("button", { name: "Optimize Game", exact: true })
    .click();
  await expect(
    page.getByText("Conservative requirement:", { exact: false }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(
    page.locator(".path-label").filter({ hasText: "Writable overlay" }),
  ).toBeVisible();
  await page.getByLabel("Discovered launch target").selectOption("bin/game");
  await expect(page.getByLabel("Executable relative to mount")).toHaveValue(
    "bin/game",
  );
  await expect(
    page.getByRole("checkbox", {
      name: "I confirm this launch target",
      exact: false,
    }),
  ).not.toBeChecked();
  await page
    .getByRole("checkbox", {
      name: "I confirm this launch target",
      exact: false,
    })
    .check();
  await page.getByRole("button", { name: "Save launch configuration" }).click();
  await page.goto("/?view=recovery&theme=light");
  await expect(
    page.getByRole("button", { name: "Reconcile stale session" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Unmount", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Remove from Library", exact: true }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "Reconcile stale session" }).click();
  await expect(
    page.getByRole("alert").filter({ hasText: "Runtime operations" }),
  ).toContainText("require the native PlaySparse app");
});
