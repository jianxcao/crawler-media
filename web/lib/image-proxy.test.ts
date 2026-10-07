import { describe, it } from "node:test";
import assert from "node:assert/strict";

describe("image-proxy url classification", () => {
  function classifyImageUrl(url: string | null): "local" | "remote" | "empty" {
    if (!url) return "empty";
    if (/^https?:\/\//i.test(url)) return "remote";
    return "local";
  }

  it("classifies /media/{id}/poster relative url as local", () => {
    const relative = "/media/11111111-1111-1111-1111-111111111111/poster";
    assert.equal(classifyImageUrl(relative), "local");
  });

  it("classifies external http url as remote", () => {
    const remote = "https://image.tmdb.org/t/p/w500/sample.jpg";
    assert.equal(classifyImageUrl(remote), "remote");
  });

  it("classifies null as empty", () => {
    assert.equal(classifyImageUrl(null), "empty");
  });
});
