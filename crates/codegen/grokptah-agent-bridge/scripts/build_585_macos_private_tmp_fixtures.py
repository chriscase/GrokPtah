#!/usr/bin/env python3
"""Build synthetic relocated macOS /private/tmp spelling fixtures from the Linux #585 cut.

These are NOT genuine historical macOS producer captures. They preserve the
original Work UUID and request id for cross-host replay tests only.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

BRIDGE = Path(__file__).resolve().parents[1]
LINUX = BRIDGE / "tests/fixtures/wr-v2-585-schema2-creation-cut-25d40262"
PRODUCER_SHA = "25d40262a3b530227af850f26974c19dfc2935fd"
OLD_WS = "/tmp/grokptah-585-cut-root.wu1Qcc/workspace"
NEW_WS = "/private/tmp/grokptah-585-cut-root.wu1Qcc/workspace"
OLD_DIGEST = "fc461db136aadc1c047675f98cc35d7af84522e1a01e7c3aa3c9908645da9ee7"
NEW_DIGEST = "f1cbe34c2d1784835b7447699f0d21e56583364e5d2203e376d616b146868c77"
OWNER_SHARD = "78ca40c60c85d711a5868dd5ff01b175c8db21b9420a4dbc8f6af261e4472d9d"
REQUEST_SHARD = "700ef99274f38ecae90addb13c78510ced67f5afef807602fc64c18bc1775651"
PROVENANCE = """fixture_kind=synthetic_relocated
not_a_genuine_historical_macos_capture=true
source_linux_fixture=wr-v2-585-schema2-creation-cut-25d40262
producer_sha=25d40262a3b530227af850f26974c19dfc2935fd
workspace_display_spelling=/private/tmp/grokptah-585-cut-root.wu1Qcc/workspace
"""


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def derive_integrity(item: dict, workspace: str) -> tuple[str, str]:
    item_path = Path(tempfile.mkstemp(suffix=".json")[1])
    item_path.write_text(json.dumps(item), encoding="utf-8")
    env = os.environ.copy()
    env["DERIVE_RELOCATED_CREATION_ITEM"] = str(item_path)
    env["DERIVE_RELOCATED_CREATION_WORKSPACE"] = workspace
    out = subprocess.run(
        [
            "cargo",
            "test",
            "--locked",
            "--test",
            "work_creation_recovery",
            "derive_relocated_creation_fixture_integrity_from_env",
            "--",
            "--exact",
            "--nocapture",
        ],
        cwd=BRIDGE,
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    item_digest = payload_hash = None
    for line in out.stdout.splitlines() + out.stderr.splitlines():
        if line.startswith("creation_item_digest="):
            item_digest = line.split("=", 1)[1]
        if line.startswith("payload_hash="):
            payload_hash = line.split("=", 1)[1]
    item_path.unlink(missing_ok=True)
    if not item_digest or not payload_hash:
        raise RuntimeError(f"failed to derive integrity hashes:\n{out.stdout}\n{out.stderr}")
    return item_digest, payload_hash


def patch_creation_json(path: Path, workspace: str, item_digest: str, payload_hash: str) -> None:
    if path.suffix != ".json":
        return
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        return
    if (
        data.get("schemaVersion") == 2
        and "status" in data
        and data.get("tool")
        and "payloadHash" in data
    ):
        data["workspaceDigest"] = NEW_DIGEST
        data["payloadHash"] = payload_hash
        work = (data.get("response") or {}).get("work")
        if isinstance(work, dict) and work.get("workspace") in {OLD_WS, NEW_WS}:
            work["workspace"] = workspace
        path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
        return
    if data.get("operation") == "create" and "item" in data:
        data["item"]["workspace"] = workspace
        data["creationItemDigest"] = item_digest
        if "request" in data:
            data["request"]["workspaceDisplay"] = workspace
            data["request"]["workspaceDigest"] = NEW_DIGEST
            data["request"]["payloadHash"] = payload_hash
            data["request"]["payloadWorkspace"] = workspace
    if data.get("workId") and data.get("workspace") in {OLD_WS, NEW_WS}:
        data["workspace"] = workspace
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def relocate_idempotency_shard(out: Path) -> None:
    old = (
        out
        / "ledger-tree/idempotency/v2"
        / OWNER_SHARD
        / OLD_DIGEST
        / f"{REQUEST_SHARD}.json"
    )
    new_dir = out / "ledger-tree/idempotency/v2" / OWNER_SHARD / NEW_DIGEST
    new = new_dir / f"{REQUEST_SHARD}.json"
    if old.is_file():
        new_dir.mkdir(parents=True, exist_ok=True)
        shutil.move(str(old), str(new))
        old_parent = old.parent
        if old_parent.is_dir() and not any(old_parent.iterdir()):
            old_parent.rmdir()


def write_hashes(out: Path) -> None:
    receipt = out / "receipt.json"
    intent = out / "intent.json"
    (out / "receipt.sha256").write_text(f"{sha256_file(receipt)}  {receipt}\n")
    (out / "intent.sha256").write_text(f"{sha256_file(intent)}  {intent}\n")
    meta = [
        "capture_kind=synthetic_relocated_macos_private_tmp",
        f"producer_sha={PRODUCER_SHA}",
        f"source_fixture={LINUX.name}",
        f"workspace_display={NEW_WS}",
    ]
    (out / "CAPTURE.meta").write_text("\n".join(meta) + "\n", encoding="utf-8")
    lines = []
    for path in sorted(out.rglob("*")):
        if path.is_file():
            lines.append(f"{sha256_file(path)}  {path}\n")
    (out / "MANIFEST.sha256").write_text("".join(lines), encoding="utf-8")


def build_pending(name: str) -> Path:
    out = BRIDGE / f"tests/fixtures/{name}"
    if out.exists():
        shutil.rmtree(out)
    shutil.copytree(LINUX, out)
    intent = json.loads((out / "intent.json").read_text(encoding="utf-8"))
    intent["item"]["workspace"] = NEW_WS
    item_digest, payload_hash = derive_integrity(intent["item"], NEW_WS)
    for path in out.rglob("*"):
        if path.is_file() and path.suffix == ".json":
            patch_creation_json(path, NEW_WS, item_digest, payload_hash)
    if (out / "fixture.json").is_file():
        fixture = json.loads((out / "fixture.json").read_text(encoding="utf-8"))
        fixture["workspace"] = NEW_WS
        (out / "fixture.json").write_text(json.dumps(fixture, indent=2) + "\n")
    relocate_idempotency_shard(out)
    receipt_path = (
        out
        / "ledger-tree/idempotency/v2"
        / OWNER_SHARD
        / NEW_DIGEST
        / f"{REQUEST_SHARD}.json"
    )
    shutil.copy2(receipt_path, out / "receipt.json")
    (out / "FIXTURE_PROVENANCE").write_text(PROVENANCE, encoding="utf-8")
    (out / "PRODUCE_COMMAND.txt").write_text(
        "\n".join(
            [
                "fixture_kind=synthetic_relocated",
                "relocated_from=" + str(LINUX),
                f"producer_sha={PRODUCER_SHA}",
                f"workspace_display={NEW_WS}",
                "state=pending_intent_cut",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    write_hashes(out)
    return out


def replay_linux_pending_to_complete() -> Path:
    tmp = Path(tempfile.mkdtemp())
    shutil.copytree(LINUX / "ledger-tree", tmp / "ledger")
    shutil.copytree(LINUX / "home-tree", tmp / "home")
    shutil.copy2(LINUX / "fixture.json", tmp / "fixture.json")
    os.makedirs(OLD_WS, exist_ok=True)
    bin_glob = [
        p
        for p in (BRIDGE / "target/debug/deps").glob("work_creation_recovery-*")
        if os.access(p, os.X_OK)
    ]
    if not bin_glob:
        subprocess.run(
            ["cargo", "test", "--locked", "--test", "work_creation_recovery", "--no-run"],
            cwd=BRIDGE,
            check=True,
        )
        bin_glob = [
            p
            for p in (BRIDGE / "target/debug/deps").glob("work_creation_recovery-*")
            if os.access(p, os.X_OK)
        ]
    env = os.environ.copy()
    env.update(
        {
            "WORK_CREATION_CHILD_ROOT": str(tmp),
            "WORK_CREATION_PHASE": "replay",
            "WORK_CREATION_STAGE": "",
            "WORK_CREATION_WORKSPACE_ALIAS": OLD_WS,
            "GROKPTAH_AGENT_OFFLINE": "1",
        }
    )
    subprocess.run(
        [str(sorted(bin_glob)[0]), "--exact", "creation_service_child", "--nocapture"],
        cwd=BRIDGE,
        env=env,
        check=True,
    )
    complete_src = Path(tempfile.mkdtemp())
    shutil.copytree(tmp / "ledger", complete_src / "ledger-tree")
    shutil.copytree(tmp / "home", complete_src / "home-tree")
    shutil.copy2(tmp / "fixture.json", complete_src / "fixture.json")
    shutil.copy2(LINUX / "PRODUCER_SHA", complete_src / "PRODUCER_SHA")
    shutil.copy2(LINUX / "PRODUCE_COMMAND.txt", complete_src / "PRODUCE_COMMAND.txt")
    shutil.copy2(LINUX / "build.log", complete_src / "build.log")
    shutil.copy2(LINUX / "cut.log", complete_src / "cut.log")
    receipt = next((complete_src / "ledger-tree/idempotency").rglob("*.json"))
    shutil.copy2(receipt, complete_src / "receipt.json")
    shutil.copy2(LINUX / "intent.json", complete_src / "intent.json")
    intent_dir = complete_src / "ledger-tree/work-lifecycle-intents"
    intent_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(
        LINUX
        / "ledger-tree/work-lifecycle-intents/c4f7670a53282d8b8f0cf66b925df099cbbaabdb8d91869ba2ad2e55dad2b882.json",
        intent_dir
        / "c4f7670a53282d8b8f0cf66b925df099cbbaabdb8d91869ba2ad2e55dad2b882.json",
    )
    return complete_src


def build_complete(name: str, complete_src: Path) -> Path:
    out = BRIDGE / f"tests/fixtures/{name}"
    if out.exists():
        shutil.rmtree(out)
    shutil.copytree(complete_src, out)
    intent = json.loads((out / "intent.json").read_text(encoding="utf-8"))
    intent["item"]["workspace"] = NEW_WS
    item_digest, payload_hash = derive_integrity(intent["item"], NEW_WS)
    for path in out.rglob("*"):
        if path.is_file() and path.suffix == ".json":
            patch_creation_json(path, NEW_WS, item_digest, payload_hash)
    fixture = json.loads((out / "fixture.json").read_text(encoding="utf-8"))
    fixture["workspace"] = NEW_WS
    (out / "fixture.json").write_text(json.dumps(fixture, indent=2) + "\n")
    relocate_idempotency_shard(out)
    receipt_path = (
        out
        / "ledger-tree/idempotency/v2"
        / OWNER_SHARD
        / NEW_DIGEST
        / f"{REQUEST_SHARD}.json"
    )
    shutil.copy2(receipt_path, out / "receipt.json")
    (out / "FIXTURE_PROVENANCE").write_text(PROVENANCE, encoding="utf-8")
    (out / "PRODUCE_COMMAND.txt").write_text(
        "\n".join(
            [
                "fixture_kind=synthetic_relocated",
                "relocated_from=" + str(LINUX),
                f"producer_sha={PRODUCER_SHA}",
                f"workspace_display={NEW_WS}",
                "state=completed_replay_once",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    write_hashes(out)
    receipt = json.loads((out / "receipt.json").read_text())
    assert receipt["status"] == "complete"
    assert receipt["workspaceDigest"] == NEW_DIGEST
    assert (
        receipt["response"]["work"]["workId"]
        == "6449ac1f-1b2c-46eb-8d4d-a922edb64ce9"
    )
    return out


def main() -> None:
    build_pending(
        "wr-v2-585-schema2-creation-cut-macos-private-tmp-pending-25d40262"
    )
    complete_src = replay_linux_pending_to_complete()
    build_complete(
        "wr-v2-585-schema2-creation-cut-macos-private-tmp-complete-25d40262",
        complete_src,
    )


if __name__ == "__main__":
    main()
