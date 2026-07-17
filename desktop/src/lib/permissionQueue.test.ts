import { describe, expect, it } from "vitest";
import {
  dequeuePermission,
  enqueuePermission,
  headPermission,
  sessionIdForPermission,
} from "./permissionQueue";
import type { PermissionRequest } from "./protocol";

function req(
  id: string,
  session_id: string,
  tool = "run_terminal_cmd",
): PermissionRequest {
  return {
    id,
    session_id,
    tool_name: tool,
    summary: `Allow ${tool}?`,
    detail: { cmd: "echo hi" },
  };
}

describe("permissionQueue (#141)", () => {
  it("queues concurrent requests without clobbering the first", () => {
    let q: PermissionRequest[] = [];
    q = enqueuePermission(q, req("a", "sess-background"));
    q = enqueuePermission(q, req("b", "sess-focused"));
    expect(q).toHaveLength(2);
    expect(headPermission(q)?.id).toBe("a");
    expect(headPermission(q)?.session_id).toBe("sess-background");
  });

  it("does not duplicate the same request id", () => {
    let q = enqueuePermission([], req("a", "s1"));
    q = enqueuePermission(q, req("a", "s1"));
    expect(q).toHaveLength(1);
  });

  it("after answering the head, the next concurrent request is shown", () => {
    let q = enqueuePermission([], req("a", "s1"));
    q = enqueuePermission(q, req("b", "s2"));
    q = dequeuePermission(q, "a");
    expect(headPermission(q)?.id).toBe("b");
    expect(headPermission(q)?.session_id).toBe("s2");
  });

  it("sessionIdForPermission prefers request.session_id over focused tab", () => {
    const r = req("p1", "sess-non-focused");
    expect(sessionIdForPermission(r, "sess-focused")).toBe("sess-non-focused");
    expect(sessionIdForPermission(r, null)).toBe("sess-non-focused");
  });
});
