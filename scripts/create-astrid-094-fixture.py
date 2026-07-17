#!/usr/bin/env python3
"""Create a sanitized copy of the frozen 2026-07-15 Astrid 0.9.4 home shape."""

from __future__ import annotations

import argparse
import json
import os
import stat
from collections import Counter
from pathlib import Path


CONSTRUCTED_COUNTS = {
    "bin": 63,
    "etc": 9,
    "home": 370,
    "keys": 8,
    "log": 7,
    "run": 5,
    "secrets": 1,
    "var": 9,
    "wit": 11,
}
RUNTIME_EXECUTABLES = {"astrid", "astrid-daemon", "astrid-build", "astrid-emit"}
SHAPE_PATH = Path(__file__).with_name("astrid-094-frozen-shape.json")


def write(root: Path, relative: str, content: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def create(root: Path) -> None:
    if root.exists() and any(root.iterdir()):
        raise ValueError(f"fixture root is not empty: {root}")
    root.mkdir(parents=True, exist_ok=True)

    for index in range(63):
        write(root, f"bin/component-{index:02}.wasm", f"wasm-{index:02}\n")

    write(root, "etc/layout-version", "1\n")
    write(root, "etc/groups.toml", "[groups]\n")
    for index in range(7):
        write(
            root,
            f"etc/profiles/principal-{index}.toml",
            f"enabled = true\nindex = {index}\n",
        )
    (root / "etc/hooks").mkdir(parents=True)

    distro_locks = {
        "alice": ("astralis", "0.2.2"),
        "bob": ("aos-ce", "2026.1.1"),
        "carol": ("unicity-ce", "2026.1.1"),
        "dan": ("other", "1.0.0"),
    }
    for principal, (identifier, version) in distro_locks.items():
        write(
            root,
            f"home/{principal}/.config/distro.lock",
            f'[distro]\nid = "{identifier}"\nversion = "{version}"\n',
        )
    for index in range(140):
        write(
            root,
            f"home/alice/.local/capsules/asset-{index:03}",
            f"capsule-payload-or-meta-{index:03}\n",
        )
    for index in range(4):
        write(
            root,
            f"home/alice/.local/audit/record-{index:03}",
            f"audit-{index:03}\n",
        )
    for index in range(26):
        write(
            root,
            f"home/alice/.config/env/override-{index:03}",
            f"ENV_{index:03}=preserved\n",
        )
    for index in range(196):
        write(
            root,
            f"home/alice/.local/state/item-{index:03}",
            f"principal-state-{index:03}\n",
        )

    write(root, "keys/runtime.key", "synthetic-runtime-key-material\n")
    for index in range(7):
        write(root, f"keys/device-{index}.key", f"synthetic-device-key-{index}\n")
    for index in range(7):
        write(root, f"log/runtime-{index}.log", f"log-{index}\n")
    for relative, content in {
        ".hud-health": "stale HUD health\n",
        "session.principal": "transient-principal\n",
        "system.lock": "daemon-lock\n",
        "system.pid": "12345\n",
        "system.token": "ephemeral-credential\n",
    }.items():
        write(root, f"run/{relative}", content)
    write(root, "secrets/providers.toml", 'token = "synthetic-secret"\n')
    for index in range(9):
        write(root, f"var/state-{index}", f"state-{index}\n")
    for index in range(11):
        write(
            root,
            f"wit/contract-{index}.wit",
            f"package fixture:contract{index};\n",
        )

    for directory in CONSTRUCTED_COUNTS:
        os.chmod(root / directory, 0o700)
    os.chmod(root, 0o700)
    os.chmod(root / "secrets", 0o755)
    for path in root.rglob("*"):
        if path.is_file():
            os.chmod(path, 0o644)
    os.chmod(root / "home/alice/.local/state/item-000", 0o755)
    validate(root)


def validate(root: Path) -> None:
    shape = json.loads(SHAPE_PATH.read_text(encoding="utf-8"))
    expected_files = expected_file_paths(shape)
    expected_directories = expected_directory_paths(expected_files, shape["empty_directories"])
    counts: Counter[str] = Counter()
    files: set[str] = set()
    directories: set[str] = set()
    for path in root.rglob("*"):
        metadata = path.lstat()
        relative = path.relative_to(root).as_posix()
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError(f"fixture contains a symlink: {path}")
        if path.is_file():
            counts[Path(relative).parts[0]] += 1
            files.add(relative)
        elif path.is_dir():
            directories.add(relative)
        else:
            raise ValueError(f"fixture contains a special file: {path}")

    expected_counts = shape["top_level_counts"]
    if dict(counts) != expected_counts:
        raise ValueError(
            f"fixture counts differ: expected {expected_counts}, found {dict(counts)}"
        )
    if len(files) != shape["total_regular_files"]:
        raise ValueError(
            f"fixture must contain {shape['total_regular_files']} regular files, found {len(files)}"
        )
    if files != expected_files:
        missing = sorted(expected_files - files)
        unexpected = sorted(files - expected_files)
        raise ValueError(f"fixture file topology differs: missing={missing}, unexpected={unexpected}")
    if directories != expected_directories:
        missing = sorted(expected_directories - directories)
        unexpected = sorted(directories - expected_directories)
        raise ValueError(
            f"fixture directory topology differs: missing={missing}, unexpected={unexpected}"
        )

    root_mode = stat.S_IMODE(root.lstat().st_mode)
    if root_mode != int(shape["root_mode"], 8):
        raise ValueError(f"fixture root mode is {root_mode:04o}, expected {shape['root_mode']}")
    for top_level, encoded_mode in shape["top_level_modes"].items():
        path = root / top_level
        mode = stat.S_IMODE(path.lstat().st_mode)
        if mode != int(encoded_mode, 8):
            raise ValueError(
                f"fixture top-level mode for {top_level} is {mode:04o}, expected {encoded_mode}"
            )
    for relative, encoded_mode in shape["representative_file_modes"].items():
        mode = stat.S_IMODE((root / relative).lstat().st_mode)
        if mode != int(encoded_mode, 8):
            raise ValueError(
                f"fixture representative mode for {relative} is {mode:04o}, expected {encoded_mode}"
            )

    bin_entries = [Path(path) for path in files if Path(path).parts[0] == "bin"]
    if len(bin_entries) != shape["bin"]["count"] or not all(
        path.suffix == ".wasm" for path in bin_entries
    ):
        raise ValueError("every frozen bin entry must be a WASM component")
    if set(shape["bin"]["forbidden"]) != RUNTIME_EXECUTABLES:
        raise ValueError("frozen manifest runtime executable exclusions changed")
    if any((root / "bin" / name).exists() for name in shape["bin"]["forbidden"]):
        raise ValueError("the frozen home must not contain managed runtime executables")
    hooks = root / "etc/hooks"
    if not hooks.is_dir() or any(hooks.iterdir()):
        raise ValueError("the frozen etc/hooks directory must be empty")


def expected_file_paths(shape: dict[str, object]) -> set[str]:
    files = set(shape["etc_files"])
    bin_shape = shape["bin"]
    files.update(
        f"bin/{bin_shape['pattern'].format(index=index)}"
        for index in range(bin_shape["count"])
    )
    home = shape["home_files"]
    files.update(home["exact"])
    for entry in home["patterns"]:
        files.update(entry["pattern"].format(index=index) for index in range(entry["count"]))
    for entries in shape["exact_files"].values():
        files.update(entries)
    return files


def expected_directory_paths(files: set[str], empty_directories: list[str]) -> set[str]:
    directories = set(empty_directories)
    for relative in files:
        parent = Path(relative).parent
        while parent != Path("."):
            directories.add(parent.as_posix())
            parent = parent.parent
    return directories


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path)
    args = parser.parse_args()
    create(args.root.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
