import { describe, expect, it } from "vitest";
import { bytes, savings, status, effectiveBytes } from "./models";
import type { Game } from "./models";
const game: Game = {
  id: "1",
  name: "Fixture",
  source: "/source",
  analysis: null,
  store: null,
  verified: false,
  store_stats: null,
  overlay_allocated_bytes: 0,
  launch: null,
  session: null,
  error: null,
};
describe("honest storage and state presentation", () => {
  it("counts overlay allocation and leaves unknown effective usage unknown", () => {
    const stored = {
      ...game,
      store_stats: {
        allocated_bytes: 80,
        logical_bytes: 100,
        physical_bytes: 70,
        files: 1,
      },
      overlay_allocated_bytes: 25,
    };
    expect(effectiveBytes(stored)).toBe(105);
    expect(savings(100, effectiveBytes(stored))).toBe(-5);
    expect(effectiveBytes({ ...stored, overlay_allocated_bytes: null })).toBe(
      null,
    );
  });
  it("never replaces missing allocation with zero or fabricated savings", () => {
    expect(bytes(null)).toBe("Unavailable");
    expect(savings(null, 20)).toBe(null);
    expect(savings(0, 20)).toBe(null);
    expect(savings(100, 125)).toBe(-25);
    expect(bytes(1024 ** 3)).toBe("1.00 GiB");
  });
  it("does not display readiness before verified publication", () => {
    expect(status(game, [])).toBe("Not analyzed");
    expect(status({ ...game, store: "/store" }, [])).toBe("Verify required");
    expect(status({ ...game, store: "/store", verified: true }, [])).toBe(
      "Ready",
    );
    expect(
      status(
        { ...game, store: "/store", verified: true, error: "store missing" },
        [],
      ),
    ).toBe("Needs attention");
  });
});
