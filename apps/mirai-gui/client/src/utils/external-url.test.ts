import { describe, expect, it } from "vitest";
import { httpUrlHostname, safeHttpUrl } from "./external-url";

describe("safeHttpUrl", () => {
  it.each([
    ["https://docs.trymirai.com/search", "https://docs.trymirai.com/search"],
    ["http://example.com/page", "http://example.com/page"],
    ["  https://example.com/a  ", "https://example.com/a"],
  ])("accepts %s", (raw, expected) => {
    expect(safeHttpUrl(raw)).toBe(expected);
  });

  it.each([
    "javascript:alert(1)",
    "JaVaScRiPt:alert(1)",
    "java\tscript:alert(1)",
    "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
    "file:///etc/passwd",
    "blob:https://example.com/1",
    "vbscript:msgbox(1)",
    "//example.com",
    "/relative/path",
    "example.com",
    "",
    "   ",
    "https://",
  ])("refuses %s", (raw) => {
    expect(safeHttpUrl(raw)).toBeUndefined();
  });

  it.each([undefined, null, 42, {}, [], true])("refuses the non-string %p", (raw) => {
    expect(safeHttpUrl(raw)).toBeUndefined();
  });

  // href is written from the parse result, not the provider's raw text.
  it("normalizes instead of echoing the input", () => {
    expect(safeHttpUrl("http://example.com/a b?x=<script>")).toBe("http://example.com/a%20b?x=%3Cscript%3E");
  });
});

describe("httpUrlHostname", () => {
  it.each([
    ["https://docs.trymirai.com/search", "docs.trymirai.com"],
    ["https://www.example.com/", "example.com"],
    ["javascript:alert(1)", ""],
    ["not a url", ""],
  ])("reduces %s to %s", (raw, expected) => {
    expect(httpUrlHostname(raw)).toBe(expected);
  });
});
