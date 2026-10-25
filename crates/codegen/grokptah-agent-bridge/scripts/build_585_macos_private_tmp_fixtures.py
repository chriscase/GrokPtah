#!/usr/bin/env python3
"""Relocate the frozen #585 Linux producer cut to old-macOS /private/tmp spelling.

Preserves work UUID, request id, payload hash, and producer SHA. Does not alter
the Linux fixture bundle.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
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


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def rewrite_text(data: str) -> str:
    return data.replace(OLD_WS, NEW_WS).replace(OLD_DIGEST, NEW_DIGEST)


def rewrite_json_file(path: Path) -> None:
    if path.suffix != ".json":
        return
    raw = path.read_text(encoding="utf-8")
    if OLD_WS not in raw and OLD_DIGEST not in raw:
        return
    path.write_text(rewrite_text(raw), encoding="utf-8")


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


def copy_linux_tree(name: str) -> Path:
    out = BRIDGE / f"tests/fixtures/{name}"
    if out.exists():
        shutil.rmtree(out)
    shutil.copytree(LINUX, out)
    for path in out.rglob("*"):
        if path.is_file() and path.suffix == ".json":
            rewrite_json_file(path)
    relocate_idempotency_shard(out)
    return out


def write_hashes(out: Path) -> None:
    receipt = out / "receipt.json"
    intent = out / "intent.json"
    (out / "receipt.sha256").write_text(f"{sha256_file(receipt)}  {receipt}\n")
    (out / "intent.sha256").write_text(f"{sha256_file(intent)}  {intent}\n")
    meta = [
        "capture_kind=macos_private_tmp_relocation",
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


def replay_linux_pending_to_complete() -> Path:
    import tempfile

    tmp = Path(tempfile.mkdtemp())
    shutil.copytree(LINUX / "ledger-tree", tmp / "ledger")
    shutil.copytree(LINUX / "home-tree", tmp / "home")
    shutil.copy2(LINUX / "fixture.json", tmp / "fixture.json")
    os.makedirs(OLD_WS, exist_ok=True)
    bin_glob = list((BRIDGE / "target/debug/deps").glob("work_creation_recovery-*"))
    bin_glob = [p for p in bin_glob if os.access(p, os.X_OK)]
    if not bin_glob:
        subprocess.run(
            ["cargo", "test", "--locked", "--test", "work_creation_recovery", "--no-run"],
            cwd=BRIDGE,
            check=True,
        )
        bin_glob = [p for p in (BRIDGE / "target/debug/deps").glob("work_creation_recovery-*") if os.access(p, os.X_OK)]
    bin_path = sorted(bin_glob)[0]
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
        [str(bin_path), "--exact", "creation_service_child", "--nocapture"],
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
    intent_in_ledger = list((complete_src / "ledger-tree/work-lifecycle-intents").glob("*.json"))
    if not intent_in_ledger:
        intent_dir = complete_src / "ledger-tree/work-lifecycle-intents"
        intent_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(
            LINUX / "ledger-tree/work-lifecycle-intents/c4f7670a53282d8b8f0cf66b925df099cbbaabdb8d91869ba2ad2e55dad2b882.json",
            intent_dir
            / "c4f7670a53282d8b8f0cf66b925df099cbbaabdb8d91869ba2ad2e55dad2b882.json",
        )
    return complete_src


def main() -> None:
    pending = copy_linux_tree(
        "wr-v2-585-schema2-creation-cut-macos-private-tmp-pending-25d40262"
    )
    (pending / "PRODUCE_COMMAND.txt").write_text(
        "\n".join(
            [
                "relocated_from=" + str(LINUX),
                f"producer_sha={PRODUCER_SHA}",
                "platform_spelling=macos_private_tmp",
                f"workspace_display={NEW_WS}",
                "state=pending_intent_cut",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    write_hashes(pending)

    complete_src = replay_linux_pending_to_complete()
    complete = BRIDGE / "tests/fixtures/wr-v2-585-schema2-creation-cut-macos-private-tmp-complete-25d40262"
    if complete.exists():
        shutil.rmtree(complete)
    shutil.copytree(complete_src, complete)
    for path in complete.rglob("*"):
        if path.is_file() and path.suffix == ".json":
            rewrite_json_file(path)
    relocate_idempotency_shard(complete)
    (complete / "PRODUCE_COMMAND.txt").write_text(
        "\n".join(
            [
                "relocated_from=" + str(LINUX),
                f"producer_sha={PRODUCER_SHA}",
                "platform_spelling=macos_private_tmp",
                f"workspace_display={NEW_WS}",
                "state=completed_replay_once",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    write_hashes(complete)
    receipt = json.loads((complete / "receipt.json").read_text())
    assert receipt["status"] == "complete"
    assert receipt["workspaceDigest"] == NEW_DIGEST
    work_id = receipt["response"]["work"]["workId"]
    assert work_id == "6449ac1f-1b2c-46eb-8d4d-a922edb64ce9"


if __name__ == "__main__":
    main()
