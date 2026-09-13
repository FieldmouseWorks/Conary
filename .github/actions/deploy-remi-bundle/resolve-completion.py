#!/usr/bin/env python3
"""Resolve release completion from the exact workflow's typed launch authority."""
import json
import sys


def resolve(requested: str, launch: object) -> str:
    if requested in ("private-candidates", "active-repopulation"):
        return requested
    if requested != "launch-authority":
        raise ValueError("unknown deployment completion mode")
    if not isinstance(launch, dict) or type(launch.get("schema_version")) is not int or launch["schema_version"] != 1:
        raise ValueError("launch status must use schema_version 1")
    gate = launch["gates"]["public_universe"]
    if gate["promotion_threshold"] != "zero_exclusions":
        raise ValueError("public universe must retain zero-exclusion promotion")
    return {"blocked": "private-candidates", "passed": "active-repopulation"}[gate["state"]]


if __name__ == "__main__":
    try:
        if len(sys.argv) != 2:
            raise ValueError("one requested completion mode is required")
        print(resolve(sys.argv[1], json.load(sys.stdin)))
    except (KeyError, TypeError, ValueError) as error:
        print(f"invalid deployment launch authority: {error}", file=sys.stderr)
        sys.exit(1)
