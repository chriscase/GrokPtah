import { describe, expect, it } from "vitest";
import { displaySessionTitle, pathBasename } from "./sessionTitle";

describe("displaySessionTitle (#130)", () => {
  it("appends cwd basename when titles collide", () => {
    const peers = [
      { id: "1", title: "Evaluate the code", cwd: "/a/repo-one" },
      { id: "2", title: "Evaluate the code", cwd: "/b/repo-two" },
    ];
    expect(displaySessionTitle(peers[0], peers)).toMatch(/^repo-one ·/);
    expect(displaySessionTitle(peers[1], peers)).toMatch(/^repo-two ·/);
    expect(displaySessionTitle(peers[0], peers)).not.toBe(
      displaySessionTitle(peers[1], peers),
    );
  });

  it("leaves unique titles alone", () => {
    const peers = [
      { id: "1", title: "Unique A", cwd: "/x" },
      { id: "2", title: "Unique B", cwd: "/y" },
    ];
    expect(displaySessionTitle(peers[0], peers)).toBe("Unique A");
  });

  it("pathBasename returns last segment", () => {
    expect(pathBasename("/foo/bar/baz")).toBe("baz");
  });
});
