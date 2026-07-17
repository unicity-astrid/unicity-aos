#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).with_name("channel_publication.py")
SPEC = importlib.util.spec_from_file_location("channel_publication", SCRIPT)
PUBLICATION = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(PUBLICATION)


def asset(asset_id: int, name: str, *, state: str = "uploaded", size: int = 100) -> dict:
    return {"id": asset_id, "name": name, "state": state, "size": size}


class ChannelPublicationTests(unittest.TestCase):
    def plan(self, generation: int, assets: list[dict]) -> dict:
        return PUBLICATION.publication_plan("stable", generation, assets)

    def test_empty_release_accepts_any_positive_initial_generation(self) -> None:
        self.assertEqual(self.plan(1, [])["transaction-floor"], 0)
        self.assertEqual(self.plan(47, [])["generation"], 47)

    def test_transaction_only_is_a_recoverable_exact_retry(self) -> None:
        transaction = asset(1, "channel-stable-7.transaction.json")
        plan = self.plan(7, [transaction])
        self.assertTrue(plan["requested-transaction-present"])
        self.assertEqual(plan["transaction-floor"], 7)

    def test_new_generation_must_advance_transaction_floor(self) -> None:
        transaction = asset(1, "channel-stable-7.transaction.json")
        self.assertEqual(self.plan(8, [transaction])["generation"], 8)
        with self.assertRaises(ValueError):
            self.plan(6, [transaction])

    def test_partial_history_at_or_below_transaction_is_repairable(self) -> None:
        assets = [
            asset(1, "channel-stable-7.transaction.json"),
            asset(2, "channel-stable-7.toml"),
        ]
        self.assertEqual(self.plan(7, assets)["history-floor"], 7)
        assets[1] = asset(2, "channel-stable-7.toml.sigstore.json")
        self.assertEqual(self.plan(7, assets)["history-floor"], 7)

    def test_history_cannot_outrun_transaction(self) -> None:
        with self.assertRaisesRegex(ValueError, "no authenticated transaction"):
            self.plan(
                7,
                [
                    asset(1, "channel-stable-7.transaction.json"),
                    asset(2, "channel-stable-8.toml"),
                ],
            )

    def test_current_pointer_interruption_matrix_is_reported(self) -> None:
        base = [asset(1, "channel-stable-7.transaction.json")]
        cases = (
            ([], False, False),
            ([asset(2, "channel.toml")], True, False),
            ([asset(2, "channel.toml.sigstore.json")], False, True),
            ([asset(2, "channel.toml"), asset(3, "channel.toml.sigstore.json")], True, True),
        )
        for current, pointer, bundle in cases:
            with self.subTest(pointer=pointer, bundle=bundle):
                plan = self.plan(7, base + current)
                self.assertEqual(plan["current-pointer-present"], pointer)
                self.assertEqual(plan["current-bundle-present"], bundle)

    def test_nonuploaded_assets_are_ignored_and_scheduled_for_cleanup(self) -> None:
        assets = [
            asset(1, "channel-stable-7.transaction.json"),
            asset(2, "channel.toml", state="starter", size=0),
            asset(3, "channel.toml.sigstore.json", state="open", size=12),
        ]
        plan = self.plan(7, assets)
        self.assertEqual(plan["cleanup-asset-ids"], [2, 3])
        self.assertFalse(plan["current-pointer-present"])
        self.assertFalse(plan["current-bundle-present"])

    def test_empty_uploaded_asset_fails_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "empty"):
            self.plan(1, [asset(1, "channel.toml", size=0)])

    def test_duplicate_uploaded_name_fails_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.plan(1, [asset(1, "channel.toml"), asset(2, "channel.toml")])

    def test_paginated_api_shape_is_flattened(self) -> None:
        transaction = asset(1, "channel-stable-7.transaction.json")
        self.assertEqual(self.plan(7, [[transaction]])["uploaded-names"], [transaction["name"]])


if __name__ == "__main__":
    unittest.main()
