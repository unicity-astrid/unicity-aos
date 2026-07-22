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
# The first command in a principal's mounted home links the admitted system
# toolchain into that principal's own rustup state. Rustup proxies then shadow
# /usr/bin through PATH without mutating the shared image.
cat >"$target_dir/usr/libexec/aos/realm-env" <<'EOF'
if [ -x /usr/bin/rustup ] && [ ! -f "${RUSTUP_HOME:-$HOME/.rustup}/.aos-system-ready" ]; then
    /usr/libexec/aos/init-rustup || printf '%s\n' 'warning: AOS system rustup initialization failed' >&2
fi
EOF
cat >"$target_dir/usr/libexec/aos/init-rustup" <<'EOF'
#!/bin/sh
set -eu

: "${HOME:=/home/agent}"
: "${CARGO_HOME:=$HOME/.cargo}"
: "${RUSTUP_HOME:=$HOME/.rustup}"
export HOME CARGO_HOME RUSTUP_HOME
marker=$RUSTUP_HOME/.aos-system-ready
test -f "$marker" && exit 0

mkdir -p "$CARGO_HOME/bin" "$RUSTUP_HOME"
if ! /usr/bin/rustup toolchain list | grep -q '^aos-system'; then
    /usr/bin/rustup toolchain link aos-system /usr
fi
/usr/bin/rustup default aos-system
for proxy in cargo cargo-clippy clippy-driver rustc rustdoc rustfmt; do
    ln -sfn /usr/bin/rustup "$CARGO_HOME/bin/$proxy"
done
: >"$marker"
EOF
chmod 0755 \
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
