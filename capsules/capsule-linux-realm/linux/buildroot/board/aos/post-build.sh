#!/bin/sh
set -eu

if [ "$#" -lt 1 ]; then
    echo "usage: $0 TARGET_DIR" >&2
    exit 64
fi

target_dir=$1
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
target_tuple=riscv64-buildroot-linux-gnu
cc="$HOST_DIR/bin/$target_tuple-gcc"
strip="$HOST_DIR/bin/$target_tuple-strip"
patchelf="$HOST_DIR/bin/patchelf"
cmake_build="$BUILD_DIR/cmake-4.3.2"
cxx_headers="$HOST_DIR/$target_tuple/include/c++/14.4.0"

if [ ! -x "$cc" ] || [ ! -x "$strip" ] || [ ! -x "$patchelf" ]; then
    echo "AOS post-build requires the Buildroot RV64 GNU toolchain" >&2
    exit 69
fi

"$cc" \
    -std=c11 -Os -Wall -Wextra -Werror \
    -march=rv64gc_zicsr_zifencei -mabi=lp64d \
    -static -fno-pie -no-pie \
    -Wl,--build-id=none \
    -o "$target_dir/init" "$script_dir/init.c"
"$strip" --strip-all "$target_dir/init"

# Buildroot intentionally removes target development files because its normal
# output is an appliance. AOS Realm is a development workbench: restore the
# admitted target sysroot and the Clang driver/resource headers after Buildroot's
# final cleanup, without copying host executables or host paths into the image.
if [ ! -x "$STAGING_DIR/usr/bin/clang-22" ]; then
    echo "AOS development image requires target Clang 22" >&2
    exit 70
fi
if [ ! -d "$STAGING_DIR/usr/lib/clang/22/include" ]; then
    echo "AOS development image requires Clang 22 resource headers" >&2
    exit 70
fi
mkdir -p \
    "$target_dir/lib" \
    "$target_dir/usr/bin" \
    "$target_dir/usr/include/c++" \
    "$target_dir/usr/include" \
    "$target_dir/usr/lib" \
    "$target_dir/usr/lib/gcc/$target_tuple/14.4.0" \
    "$target_dir/usr/libexec/aos" \
    "$target_dir/usr/share/cmake-4.3/Modules"
if [ ! -f "$target_dir/lib/ld-linux-riscv64-lp64d.so.1" ]; then
    echo "AOS development image requires the RISC-V LP64D glibc loader" >&2
    exit 70
fi
cp -a "$STAGING_DIR/usr/include/." "$target_dir/usr/include/"
if [ ! -f "$cxx_headers/vector" ]; then
    echo "AOS development image requires GCC 14.4.0 C++ headers" >&2
    exit 70
fi
cp -a "$cxx_headers/." "$target_dir/usr/include/c++/14.4.0/"
cp -a "$STAGING_DIR/usr/lib/clang" "$target_dir/usr/lib/"
# Buildroot exposes target CMake only as a dependency of ctest and removes its
# frontend after installation. Restore the target binary and complete module
# tree so agents can configure ordinary CMake projects inside the Realm.
if [ ! -x "$cmake_build/bin/cmake" ] || \
    [ ! -f "$cmake_build/Modules/CMakeDetermineSystem.cmake" ]; then
    echo "AOS development image requires target CMake 4.3.2" >&2
    exit 70
fi
cp -L "$cmake_build/bin/cmake" "$target_dir/usr/bin/cmake"
cp -a "$cmake_build/Modules/." "$target_dir/usr/share/cmake-4.3/Modules/"
"$strip" --strip-all "$target_dir/usr/bin/cmake"
"$patchelf" --remove-rpath "$target_dir/usr/bin/cmake"
# Dereference the staging symlink if Buildroot represents the versioned driver
# as one. The Realm must retain the real target ELF after staging disappears.
cp -L "$STAGING_DIR/usr/bin/clang-22" "$target_dir/usr/libexec/aos/clang-22"
"$strip" --strip-all "$target_dir/usr/libexec/aos/clang-22"
"$patchelf" --remove-rpath "$target_dir/usr/libexec/aos/clang-22"
cat >"$target_dir/usr/bin/clang" <<'EOF'
#!/bin/sh
exec /usr/libexec/aos/clang-22 \
    --target=riscv64-buildroot-linux-gnu \
    --sysroot=/ \
    --gcc-install-dir=/usr/lib/gcc/riscv64-buildroot-linux-gnu/14.4.0 \
    -resource-dir=/usr/lib/clang/22 \
    -march=rv64gc_zicsr_zifencei \
    -mabi=lp64d \
    "$@"
EOF
cat >"$target_dir/usr/bin/clang++" <<'EOF'
#!/bin/sh
exec /usr/libexec/aos/clang-22 \
    --driver-mode=g++ \
    --target=riscv64-buildroot-linux-gnu \
    --sysroot=/ \
    --gcc-install-dir=/usr/lib/gcc/riscv64-buildroot-linux-gnu/14.4.0 \
    -resource-dir=/usr/lib/clang/22 \
    -march=rv64gc_zicsr_zifencei \
    -mabi=lp64d \
    "$@"
EOF
cat >"$target_dir/usr/bin/clang-cpp" <<'EOF'
#!/bin/sh
exec /usr/libexec/aos/clang-22 \
    --driver-mode=cpp \
    --target=riscv64-buildroot-linux-gnu \
    --sysroot=/ \
    --gcc-install-dir=/usr/lib/gcc/riscv64-buildroot-linux-gnu/14.4.0 \
    -resource-dir=/usr/lib/clang/22 \
    -march=rv64gc_zicsr_zifencei \
    -mabi=lp64d \
    "$@"
EOF
chmod 0755 \
    "$target_dir/usr/bin/clang" \
    "$target_dir/usr/bin/clang++" \
    "$target_dir/usr/bin/clang-cpp"
ln -sfn clang "$target_dir/usr/bin/clang-22"
ln -sfn clang "$target_dir/usr/bin/cc"
ln -sfn clang++ "$target_dir/usr/bin/c++"
for object in crt1.o crti.o crtn.o Scrt1.o; do
    if [ ! -f "$STAGING_DIR/usr/lib/$object" ]; then
        echo "AOS development image requires glibc startup object: $object" >&2
        exit 70
    fi
    cp -a "$STAGING_DIR/usr/lib/$object" "$target_dir/usr/lib/"
done
for archive in \
    libc.a \
    libdl.a \
    libm.a \
    libpthread.a \
    libresolv.a \
    librt.a
do
    if [ ! -f "$STAGING_DIR/usr/lib/$archive" ]; then
        echo "AOS development image requires glibc link input: $archive" >&2
        exit 70
    fi
    cp -a "$STAGING_DIR/usr/lib/$archive" "$target_dir/usr/lib/"
done
# A dynamic development sysroot also needs glibc's linker script, its
# non-shared support archive, and the libm development symlink. Without
# `libc.so`, `-lc` silently selects `libc.a` and produces a broken hybrid PIE
# that relocates its own now-read-only GNU_RELRO pages at startup.
for input in libc.so libc_nonshared.a libm.so; do
    if [ ! -e "$STAGING_DIR/usr/lib/$input" ]; then
        echo "AOS development image requires glibc shared link input: $input" >&2
        exit 70
    fi
    cp -a "$STAGING_DIR/usr/lib/$input" "$target_dir/usr/lib/"
done
# Rust's GNU/Linux target still requests these historical glibc libraries by
# their unversioned development names. glibc 2.34 merged their implementations
# into libc, but retains versioned compatibility DSOs. Buildroot removes the
# unversioned links from an appliance target, so restore only links to the
# already admitted runtime objects.
for link_target in \
    'libdl.so ../../lib/libdl.so.2' \
    'libpthread.so ../../lib/libpthread.so.0' \
    'librt.so ../../lib/librt.so.1' \
    'libutil.so ../../lib/libutil.so.1'
do
    link=${link_target%% *}
    target=${link_target#* }
    if [ ! -e "$target_dir/usr/lib/$target" ]; then
        echo "AOS development image requires glibc compatibility DSO: $target" >&2
        exit 70
    fi
    ln -sfn "$target" "$target_dir/usr/lib/$link"
done
if [ ! -f "$STAGING_DIR/lib/libatomic.a" ]; then
    echo "AOS development image requires GCC runtime link input: libatomic.a" >&2
    exit 70
fi
cp -a "$STAGING_DIR/lib/libatomic.a" "$target_dir/lib/"
gcc_support="$HOST_DIR/lib/gcc/$target_tuple/14.4.0"
target_gcc_support="$target_dir/usr/lib/gcc/$target_tuple/14.4.0"
for input in crtbegin.o crtbeginS.o crtbeginT.o crtend.o crtendS.o libgcc.a libgcc_eh.a; do
    if [ ! -f "$gcc_support/$input" ]; then
        echo "AOS development image requires GCC support input: $input" >&2
        exit 70
    fi
    cp -a "$gcc_support/$input" "$target_gcc_support/"
done
# Keep the workbench focused on compiling applications, not LLVM plugins. The
# Clang driver needs libclang-cpp and libLLVM, but not libclang or their SDK
# headers; dropping the latter saves tens of MiB from every Realm.
rm -rf \
    "$target_dir/usr/include/clang" \
    "$target_dir/usr/include/clang-c" \
    "$target_dir/usr/include/llvm" \
    "$target_dir/usr/include/llvm-c"
rm -f "$target_dir"/usr/lib/libclang.so "$target_dir"/usr/lib/libclang.so.*

# Python's sysconfig is build metadata in a normal Buildroot appliance. Here it
# is a live developer interface used by extension builds, so translate the
# cross-builder paths to tools and directories that exist inside the Realm.
python_config_dir="$target_dir/usr/lib/python3.14"
python_sysconfig="$python_config_dir/_sysconfigdata__linux_riscv64-linux-gnu.py"
if [ ! -f "$python_sysconfig" ]; then
    echo "AOS development image requires Python 3.14 sysconfig metadata" >&2
    exit 70
fi
for config_file in \
    "$python_config_dir/config-3.14-riscv64-linux-gnu/Makefile" \
    "$python_config_dir/_sysconfig_vars__linux_riscv64-linux-gnu.json" \
    "$python_sysconfig"
do
    sed -i \
        -e "s|$HOST_DIR/bin/../$target_tuple/sysroot||g" \
        -e "s|$STAGING_DIR||g" \
        -e "s|$HOST_DIR/bin/$target_tuple-gcc-ar|/usr/bin/ar|g" \
        -e "s|$HOST_DIR/bin/$target_tuple-gcc-nm|/usr/bin/nm|g" \
        -e "s|$HOST_DIR/bin/$target_tuple-gcc-ranlib|/usr/bin/ranlib|g" \
        -e "s|$HOST_DIR/bin/$target_tuple-g++|/usr/bin/c++|g" \
        -e "s|$HOST_DIR/bin/$target_tuple-gcc|/usr/bin/cc|g" \
        -e "s|$HOST_DIR/bin/$target_tuple-cpp|/usr/bin/clang-cpp|g" \
        -e "s|$HOST_DIR/bin/$target_tuple-|/usr/bin/|g" \
        -e "s|$HOST_DIR/bin/pkg-config|/usr/bin/pkg-config|g" \
        -e "s|$HOST_DIR/bin/python3|/usr/bin/python3|g" \
        -e "s|$BUILD_DIR/python3-3.14.6|/usr/lib/python3.14/config-3.14-riscv64-linux-gnu|g" \
        "$config_file"
done
rm -f "$python_config_dir"/__pycache__/_sysconfigdata__linux_riscv64-linux-gnu.*.pyc
"$HOST_DIR/bin/python3" -m compileall -q -f \
    -s "$target_dir" -p / "$python_sysconfig"

# GCC ships this helper for a host GDB that is not present in the Realm. It
# embeds builder paths and has no guest-side consumer.
rm -f "$target_dir"/usr/lib/libstdc++.so.*-gdb.py

# Bash sources this immutable file for every non-interactive Realm command.
# The first command links the admitted system toolchain into a private,
# guest-native rustup home. The Plan 9-backed principal home deliberately does
# not expose symlink creation, so Cargo's persistent cache stays there while
# rustup's system-toolchain link lives in the principal's resident guest RAM.
cat >"$target_dir/usr/libexec/aos/realm-env" <<'EOF'
if [ ! -f "${CARGO_HOME:-/run/aos/cargo}/.aos-runtime-ready" ]; then
    /usr/libexec/aos/init-cargo || printf '%s\n' 'warning: AOS Cargo runtime initialization failed' >&2
fi
if [ -x /usr/bin/rustup ] && [ ! -f "${RUSTUP_HOME:-/run/aos/rustup}/.aos-system-ready" ]; then
    /usr/libexec/aos/init-rustup || printf '%s\n' 'warning: AOS system rustup initialization failed' >&2
fi
EOF
cat >"$target_dir/usr/libexec/aos/init-cargo" <<'EOF'
#!/bin/sh
set -eu

: "${HOME:=/home/agent}"
: "${CARGO_HOME:=/run/aos/cargo}"
: "${CARGO_INSTALL_ROOT:=$HOME/.cargo}"
export HOME CARGO_HOME CARGO_INSTALL_ROOT
marker=$CARGO_HOME/.aos-runtime-ready
test -f "$marker" && exit 0

ensure_link() {
    target=$1
    link=$2
    if [ -L "$link" ] && [ "$(readlink "$link")" = "$target" ]; then
        return 0
    fi
    if [ -e "$link" ] || [ -L "$link" ]; then
        printf 'refusing unexpected Cargo runtime path: %s\n' "$link" >&2
        exit 70
    fi
    ln -s "$target" "$link"
}

mkdir -p "$CARGO_HOME" \
    "$CARGO_INSTALL_ROOT/bin" \
    "$CARGO_INSTALL_ROOT/git" \
    "$CARGO_INSTALL_ROOT/registry"
ensure_link "$CARGO_INSTALL_ROOT/bin" "$CARGO_HOME/bin"
ensure_link "$CARGO_INSTALL_ROOT/git" "$CARGO_HOME/git"
ensure_link "$CARGO_INSTALL_ROOT/registry" "$CARGO_HOME/registry"
for state_file in config config.toml credentials credentials.toml; do
    ensure_link "$CARGO_INSTALL_ROOT/$state_file" "$CARGO_HOME/$state_file"
done
: >"$marker"
EOF
cat >"$target_dir/usr/libexec/aos/init-rustup" <<'EOF'
#!/bin/sh
set -eu

: "${HOME:=/home/agent}"
: "${CARGO_HOME:=/run/aos/cargo}"
: "${RUSTUP_HOME:=/run/aos/rustup}"
export HOME CARGO_HOME RUSTUP_HOME
marker=$RUSTUP_HOME/.aos-system-ready
test -f "$marker" && exit 0

mkdir -p "$CARGO_HOME/bin" "$RUSTUP_HOME"
if ! /usr/bin/rustup toolchain list | grep -q '^aos-system'; then
    /usr/bin/rustup toolchain link aos-system /usr
fi
/usr/bin/rustup default aos-system
: >"$marker"
EOF
chmod 0755 \
    "$target_dir/usr/libexec/aos/init-cargo" \
    "$target_dir/usr/libexec/aos/init-rustup" \
    "$target_dir/usr/libexec/aos/realm-env"

# The upstream installers leave uninstall transaction logs whose source and
# destination entries name Buildroot's disposable output directory. Linked
# rustup toolchains do not consume these files, and the immutable system
# toolchain cannot be uninstalled from inside a Realm, so do not ship them.
rm -f \
    "$target_dir/usr/lib/rustlib/install.log" \
    "$target_dir/usr/lib/rustlib/manifest-"* \
    "$target_dir/usr/lib/rustlib/uninstall.sh"

# Buildroot's target-finalize strip pass corrupts the upstream RV64 rust-lld:
# the resulting ELF still parses, but glibc resolves an empty dynamic symbol
# and exits 127 before linking wasm32 output. Restore the already-stripped,
# release-signed upstream binary after target finalization. Its relative RPATH
# resolves only the immutable Rust libraries adjacent to it in /usr.
rust_lld_source=$BUILD_DIR/aos-rust-toolchain-1.97.1/rustc/lib/rustlib/riscv64gc-unknown-linux-gnu/bin/rust-lld
rust_lld_target=$target_dir/usr/lib/rustlib/riscv64gc-unknown-linux-gnu/bin/rust-lld
development_lock=$script_dir/../../../DEVELOPMENT.lock
expected_rust_lld=$(sed -n 's/^rust_lld_shipped_sha256=//p' "$development_lock")
if [ -z "$expected_rust_lld" ] || [ ! -x "$rust_lld_source" ]; then
    echo "AOS development image requires the pinned upstream rust-lld" >&2
    exit 70
fi
install -m 0755 "$rust_lld_source" "$rust_lld_target"
actual_rust_lld=$(sha256sum "$rust_lld_target" | cut -d ' ' -f 1)
if [ "$actual_rust_lld" != "$expected_rust_lld" ]; then
    echo "shipped rust-lld digest mismatch: expected $expected_rust_lld, got $actual_rust_lld" >&2
    exit 70
fi
if ! "$HOST_DIR/bin/$target_tuple-readelf" -d "$rust_lld_target" |
    grep -qF 'Library rpath: [$ORIGIN/../lib]'; then
    echo "shipped rust-lld lost its relative immutable-library RPATH" >&2
    exit 70
fi

leaked_path=$(LC_ALL=C grep -RIl -F "$BASE_DIR" "$target_dir" 2>/dev/null | head -n 1 || true)
if [ -n "$leaked_path" ]; then
    echo "AOS development image retains a builder path: $leaked_path" >&2
    exit 70
fi
cat >"$target_dir/usr/lib/os-release" <<'EOF'
NAME="AOS Realm"
ID=aos-realm
ID_LIKE=linux
VERSION="dev-2026.07"
VERSION_ID="2026.07"
VARIANT="Agent Workbench"
VARIANT_ID=agent-workbench
PRETTY_NAME="AOS Realm agent-workbench 2026.07"
EOF
if [ ! -x "$target_dir/usr/bin/rustc" ] || \
    [ ! -x "$target_dir/usr/bin/cargo" ] || \
    [ ! -d "$target_dir/usr/lib/rustlib/wasm32-unknown-unknown/lib" ]; then
    echo "AOS development image requires Rust, Cargo, and wasm32-unknown-unknown" >&2
    exit 70
fi
mkdir -p "$target_dir/home/agent" "$target_dir/workspace" "$target_dir/tmp"
chmod 0755 "$target_dir/init"
chmod 0700 "$target_dir/home/agent"
chmod 0700 "$target_dir/workspace"
chmod 1777 "$target_dir/tmp"
