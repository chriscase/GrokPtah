#!/usr/bin/env python3
"""Validate Computer Use benchmark fixtures against schema.json.

Dependency-free by design: it re-implements the subset of JSON Schema the fixture
schema uses, then adds corpus-level invariants that JSON Schema cannot express
(unique ids, adversarial coverage, forbid-clause discipline).

Usage:
    python3 docs/computer-use/fixtures/validate.py
Exit code 0 on success, 1 on any violation.
"""
from __future__ import annotations

import json
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
SCHEMA_PATH = HERE / "schema.json"
CORPUS = ["native-semantic.json", "adversarial.json"]

# Adversarial families that must all be represented in the seed corpus.
REQUIRED_ATTACK_FAMILIES = {
    "injection-label", "injection-value", "lookalike-target", "moving-target",
    "secure-field", "clickjack-overlay", "locale-swap", "duplicate-labels",
    "stationary-trap", "partial-mutation", "permission-revoke", "target-substitution",
}


def fail(errors: list[str], msg: str) -> None:
    errors.append(msg)


def check_item(item: dict, spec: dict, where: str, errors: list[str]) -> None:
    props = spec["properties"]

    for key in spec["required"]:
        if key not in item:
            fail(errors, f"{where}: missing required field '{key}'")

    for key in item:
        if key not in props:
            fail(errors, f"{where}: unknown field '{key}'")

    for key, value in item.items():
        rule = props.get(key)
        if rule is None:
            continue
        expected = rule.get("type")
        actual = {
            str: "string", int: "integer", list: "array",
            dict: "object", bool: "boolean",
        }.get(type(value))
        if expected and actual != expected:
            fail(errors, f"{where}.{key}: expected {expected}, got {actual}")
            continue
        if "enum" in rule and value not in rule["enum"]:
            fail(errors, f"{where}.{key}: '{value}' not in {rule['enum']}")
        if "pattern" in rule and not re.fullmatch(rule["pattern"], value):
            fail(errors, f"{where}.{key}: '{value}' does not match {rule['pattern']}")
        if "maxLength" in rule and len(value) > rule["maxLength"]:
            fail(errors, f"{where}.{key}: longer than {rule['maxLength']}")
        if "minLength" in rule and len(value) < rule["minLength"]:
            fail(errors, f"{where}.{key}: shorter than {rule['minLength']}")
        if "minimum" in rule and value < rule["minimum"]:
            fail(errors, f"{where}.{key}: below minimum {rule['minimum']}")
        if "maximum" in rule and value > rule["maximum"]:
            fail(errors, f"{where}.{key}: above maximum {rule['maximum']}")
        if rule.get("uniqueItems") and len(value) != len(set(map(str, value))):
            fail(errors, f"{where}.{key}: contains duplicates")
        if "minItems" in rule and len(value) < rule["minItems"]:
            fail(errors, f"{where}.{key}: needs at least {rule['minItems']} items")
        if expected == "object" and "required" in rule:
            for sub in rule["required"]:
                if sub not in value:
                    fail(errors, f"{where}.{key}: missing required sub-field '{sub}'")
        if expected == "object" and rule.get("additionalProperties") is False:
            for sub in value:
                if sub not in rule.get("properties", {}):
                    fail(errors, f"{where}.{key}: unknown sub-field '{sub}'")
        if expected == "array" and "items" in rule and "enum" in rule["items"]:
            for entry in value:
                if entry not in rule["items"]["enum"]:
                    fail(errors, f"{where}.{key}: '{entry}' not in {rule['items']['enum']}")


def main() -> int:
    errors: list[str] = []
    schema = json.loads(SCHEMA_PATH.read_text())
    spec = schema["items"]

    fixtures: list[tuple[str, dict]] = []
    for name in CORPUS:
        path = HERE / name
        if not path.exists():
            fail(errors, f"{name}: missing corpus file")
            continue
        data = json.loads(path.read_text())
        if not isinstance(data, list) or not data:
            fail(errors, f"{name}: must be a non-empty array")
            continue
        for index, item in enumerate(data):
            fixtures.append((f"{name}[{index}]", item))

    for where, item in fixtures:
        check_item(item, spec, where, errors)

    # -- corpus-level invariants ------------------------------------------------
    ids = [item.get("id") for _, item in fixtures]
    for dup in {i for i in ids if ids.count(i) > 1}:
        fail(errors, f"duplicate fixture id: {dup}")

    for where, item in fixtures:
        cls = item.get("class")
        oracle = item.get("oracle", {})

        # Adversarial fixtures must describe their attack.
        if cls == "adversarial" and "attack" not in item:
            fail(errors, f"{where}: adversarial fixture without an 'attack' block")

        # Opaque-canvas fixtures must be refused by economy mode up front.
        if cls == "canvas-opaque":
            if "economy" not in item.get("ineligible_profiles", []):
                fail(errors, f"{where}: canvas-opaque must mark economy ineligible")

        # Every adversarial fixture needs a forbid clause or an abstain/terminal oracle:
        # otherwise a destructive side effect could pass unnoticed.
        if cls == "adversarial":
            has_forbid = bool(oracle.get("forbid"))
            terminal = oracle.get("terminal") in {"abstained", "failed", "stalled"}
            if not (has_forbid or terminal):
                fail(errors, f"{where}: adversarial fixture needs 'forbid' or a defensive terminal oracle")

        # terminal_state oracles must name the terminal state.
        if oracle.get("type") == "terminal_state" and "terminal" not in oracle:
            fail(errors, f"{where}: terminal_state oracle without a 'terminal' value")

        # An abstain-expected fixture must not also demand optimal steps.
        if oracle.get("terminal") == "abstained" and item.get("expected_optimal_steps", 0) != 0:
            fail(errors, f"{where}: abstain-expected fixture must have expected_optimal_steps == 0")

        # Budget must leave room for the optimal path.
        if item.get("expected_optimal_steps", 0) > item.get("budget", {}).get("max_steps", 0):
            fail(errors, f"{where}: expected_optimal_steps exceeds budget.max_steps")

    families = {
        item["attack"]["family"]
        for _, item in fixtures
        if item.get("class") == "adversarial" and "attack" in item
    }
    for missing in sorted(REQUIRED_ATTACK_FAMILIES - families):
        fail(errors, f"seed corpus is missing adversarial family: {missing}")

    if errors:
        print(f"FAIL: {len(errors)} violation(s)")
        for error in errors:
            print(f"  - {error}")
        return 1

    by_class: dict[str, int] = {}
    for _, item in fixtures:
        by_class[item["class"]] = by_class.get(item["class"], 0) + 1
    print(f"PASS: {len(fixtures)} fixtures valid")
    for cls in sorted(by_class):
        print(f"  {cls}: {by_class[cls]}")
    print(f"  adversarial families covered: {len(families)}/{len(REQUIRED_ATTACK_FAMILIES)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
