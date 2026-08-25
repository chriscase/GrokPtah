import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  EXTERNAL_WORKER_LIST_DEFAULT_LIMIT,
  EXTERNAL_WORKER_LIST_INCLUDE_ARCHIVED_DEFAULT,
  EXTERNAL_WORKER_LIST_MAX_LIMIT,
  GROKPTAH_BROKER_EXTERNAL_WORKER_ROUTES,
  grokptahBrokerExternalWorkerArchivePath,
  grokptahBrokerExternalWorkerListPath,
  grokptahBrokerExternalWorkerRoute,
  grokptahBrokerExternalWorkerUnarchivePath,
} from "./grokptahBrokerRoutes";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../../");

describe("external-worker broker route catalog", () => {
  it("documents list, archive, and unarchive in the published protocol table", () => {
    const protocol = readFileSync(resolve(repoRoot, "docs/WEB_BROKER_PROTOCOL.md"), "utf8");
    for (const route of GROKPTAH_BROKER_EXTERNAL_WORKER_ROUTES) {
      expect(protocol).toContain(`\`${route.method} ${route.path}\``);
    }
    expect(protocol).toMatch(/includeArchived.*omitted.*false/i);
    expect(protocol).toMatch(/Archive is never implied by cancel/i);
  });

  it("pins identity-only list query, summary, and page in the published schema", () => {
    const schema = JSON.parse(
      readFileSync(resolve(repoRoot, "docs/schemas/grokptah-external-worker.v1.schema.json"), "utf8"),
    ) as {
      $defs: Record<string, {
        additionalProperties?: boolean;
        properties?: Record<string, { default?: unknown; maximum?: number; minimum?: number; maxItems?: number }>;
        required?: string[];
      }>;
    };
    expect(schema.$defs.listQuery?.additionalProperties).toBe(false);
    expect(schema.$defs.listQuery?.properties?.limit?.minimum).toBe(1);
    expect(schema.$defs.listQuery?.properties?.limit?.maximum).toBe(EXTERNAL_WORKER_LIST_MAX_LIMIT);
    expect(schema.$defs.listQuery?.properties?.includeArchived?.default).toBe(
      EXTERNAL_WORKER_LIST_INCLUDE_ARCHIVED_DEFAULT,
    );
    expect(schema.$defs.summary?.required).toEqual(
      expect.arrayContaining(["provider", "externalAgentId", "state", "createdAt", "updatedAt"]),
    );
    expect(schema.$defs.summary?.properties).not.toHaveProperty("repository");
    expect(schema.$defs.summary?.properties).not.toHaveProperty("startingRef");
    expect(schema.$defs.listPage?.required).toContain("items");
    expect(schema.$defs.listPage?.properties?.items?.maxItems).toBe(EXTERNAL_WORKER_LIST_MAX_LIMIT);
  });

  it("keeps list read-only and archive/unarchive on the mutating CSRF path", () => {
    const list = grokptahBrokerExternalWorkerRoute("list");
    expect(list.method).toBe("GET");
    expect(list.csrf).toBe(false);
    expect(list.idempotency).toBe(false);
    expect(list.defaultClass).toBe("read-only");
    expect(list.capability).toBe("session.observe");

    for (const id of ["archive", "unarchive", "cancel"] as const) {
      const route = grokptahBrokerExternalWorkerRoute(id);
      expect(route.method).toBe("POST");
      expect(route.csrf).toBe(true);
      expect(route.idempotency).toBe(true);
      expect(route.defaultClass).toBe("execute");
      expect(route.capability).toBe("run.execute");
    }

    expect(grokptahBrokerExternalWorkerRoute("archive").path).not.toContain("/cancel");
    expect(grokptahBrokerExternalWorkerRoute("cancel").path).not.toContain("/archive");
    expect(grokptahBrokerExternalWorkerArchivePath("b", "a")).not.toBe(
      grokptahBrokerExternalWorkerUnarchivePath("b", "a"),
    );
  });

  it("always serializes includeArchived and bounds list pages", () => {
    expect(EXTERNAL_WORKER_LIST_INCLUDE_ARCHIVED_DEFAULT).toBe(false);
    expect(EXTERNAL_WORKER_LIST_DEFAULT_LIMIT).toBe(20);
    expect(grokptahBrokerExternalWorkerListPath("binding-1")).toBe(
      "/bindings/binding-1/external-workers?includeArchived=false",
    );
    expect(grokptahBrokerExternalWorkerListPath("binding-1", { limit: 1, includeArchived: false }))
      .toBe("/bindings/binding-1/external-workers?limit=1&includeArchived=false");
    expect(grokptahBrokerExternalWorkerListPath("binding-1", {
      cursor: "agent-2",
      includeArchived: true,
    })).toBe("/bindings/binding-1/external-workers?cursor=agent-2&includeArchived=true");
  });
});
