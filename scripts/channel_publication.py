#!/usr/bin/env python3
"""Validate GitHub channel assets and plan one publication attempt."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


CHANNELS = ("stable", "dev", "nightly")
MAX_GENERATION = 999_999_999_999_999_999


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def flatten_assets(value: Any) -> list[dict[str, Any]]:
    require(isinstance(value, list), "GitHub assets response must be an array")
    flattened: list[Any] = []
    for item in value:
        if isinstance(item, list):
            flattened.extend(item)
        else:
            flattened.append(item)
    require(all(isinstance(item, dict) for item in flattened), "every asset must be an object")
    return flattened


def publication_plan(channel: str, generation: int, value: Any) -> dict[str, Any]:
    require(channel in CHANNELS, "channel must be stable, dev, or nightly")
    require(
        type(generation) is int and 0 < generation <= MAX_GENERATION,
        f"generation must be between 1 and {MAX_GENERATION}",
    )
    assets = flatten_assets(value)
    uploaded: dict[str, dict[str, Any]] = {}
    cleanup_ids: list[int] = []
    for asset in assets:
        asset_id = asset.get("id")
        name = asset.get("name")
        state = asset.get("state")
        size = asset.get("size")
        require(type(asset_id) is int and asset_id > 0, "asset id must be a positive integer")
        require(isinstance(name, str) and name != "", "asset name must be non-empty")
        require(isinstance(state, str) and state != "", f"asset {name!r} has no state")
        require(type(size) is int and size >= 0, f"asset {name!r} has an invalid size")
        if state != "uploaded":
            cleanup_ids.append(asset_id)
            continue
        require(size > 0, f"uploaded asset {name!r} is empty; repair it manually")
        require(name not in uploaded, f"duplicate uploaded asset name {name!r}")
        uploaded[name] = asset

    transaction_pattern = re.compile(
        rf"^channel-{re.escape(channel)}-([1-9][0-9]{{0,17}})\.transaction\.json$"
    )
    history_pattern = re.compile(
        rf"^channel-{re.escape(channel)}-([1-9][0-9]{{0,17}})\.toml(?:\.sigstore\.json)?$"
    )
    transaction_generations = sorted(
        int(match.group(1))
        for name in uploaded
        if (match := transaction_pattern.fullmatch(name)) is not None
    )
    history_generations = sorted(
        int(match.group(1))
        for name in uploaded
        if (match := history_pattern.fullmatch(name)) is not None
    )
    transaction_floor = transaction_generations[-1] if transaction_generations else 0
    history_floor = history_generations[-1] if history_generations else 0
    require(
        history_floor <= transaction_floor,
        f"history generation {history_floor} has no authenticated transaction",
    )
    requested_transaction = f"channel-{channel}-{generation}.transaction.json"
    requested_transaction_present = requested_transaction in uploaded
    if requested_transaction_present:
        require(
            generation == transaction_floor,
            "an existing requested transaction must be the authenticated floor",
        )
    elif transaction_floor != 0:
        require(
            generation > transaction_floor,
            f"new generation must be greater than transaction floor {transaction_floor}",
        )

    return {
        "schema-version": 1,
        "channel": channel,
        "generation": generation,
        "uploaded-names": sorted(uploaded),
        "cleanup-asset-ids": sorted(cleanup_ids),
        "transaction-floor": transaction_floor,
        "history-floor": history_floor,
        "requested-transaction-present": requested_transaction_present,
        "current-pointer-present": "channel.toml" in uploaded,
        "current-bundle-present": "channel.toml.sigstore.json" in uploaded,
    }


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("--channel", choices=CHANNELS, required=True)
    root.add_argument("--generation", type=int, required=True)
    root.add_argument("--assets", type=Path, required=True)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    with args.assets.open(encoding="utf-8") as source:
        value = json.load(source)
    print(json.dumps(publication_plan(args.channel, args.generation, value), sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"channel publication: {error}", file=sys.stderr)
        raise SystemExit(1)
