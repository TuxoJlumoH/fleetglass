import { describe, expect, it } from "vitest";
import { formatAge } from "./age";

const NOW = Date.parse("2026-11-03T12:00:00Z");

describe("formatAge", () => {
  it.each([
    ["2026-11-03T11:59:15Z", "45s"],
    ["2026-11-03T11:48:00Z", "12m"],
    ["2026-11-03T09:00:00Z", "3h"],
    ["2026-11-01T11:07:17Z", "2d"],
    ["2024-10-01T00:00:00Z", "2y"],
    ["2026-11-03T12:00:05Z", "0s"],
  ])("formats %s as %s", (ts, expected) => {
    expect(formatAge(ts, NOW)).toBe(expected);
  });

  it("keeps unparseable values as they are", () => {
    expect(formatAge("not a date", NOW)).toBe("not a date");
    expect(formatAge("", NOW)).toBe("—");
  });
});
