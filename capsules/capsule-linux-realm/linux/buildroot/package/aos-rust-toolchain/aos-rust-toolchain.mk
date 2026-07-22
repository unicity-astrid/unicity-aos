################################################################################
#
# aos-rust-toolchain
#
################################################################################

AOS_RUST_TOOLCHAIN_VERSION = 1.97.1
AOS_RUST_TOOLCHAIN_SITE = https://static.rust-lang.org/dist
AOS_RUST_TOOLCHAIN_SOURCE = rust-$(AOS_RUST_TOOLCHAIN_VERSION)-riscv64gc-unknown-linux-gnu.tar.xz
AOS_RUST_TOOLCHAIN_WASM_STD = rust-std-$(AOS_RUST_TOOLCHAIN_VERSION)-wasm32-unknown-unknown.tar.xz
AOS_RUST_TOOLCHAIN_EXTRA_DOWNLOADS = $(AOS_RUST_TOOLCHAIN_WASM_STD)
AOS_RUST_TOOLCHAIN_LICENSE = Apache-2.0 or MIT
AOS_RUST_TOOLCHAIN_LICENSE_FILES = LICENSE-APACHE LICENSE-MIT

define AOS_RUST_TOOLCHAIN_EXTRACT_WASM_STD
	mkdir -p $(@D)/wasm-std
	$(call suitable-extractor,$(AOS_RUST_TOOLCHAIN_WASM_STD)) \
		$(AOS_RUST_TOOLCHAIN_DL_DIR)/$(AOS_RUST_TOOLCHAIN_WASM_STD) | \
		$(TAR) -C $(@D)/wasm-std $(TAR_OPTIONS) -
endef

AOS_RUST_TOOLCHAIN_POST_EXTRACT_HOOKS += AOS_RUST_TOOLCHAIN_EXTRACT_WASM_STD

define AOS_RUST_TOOLCHAIN_INSTALL_TARGET_CMDS
	(cd $(@D); \
		./install.sh \
			--prefix=/usr \
			--destdir=$(TARGET_DIR) \
			--disable-ldconfig \
			--components=rustc,cargo,rust-std-riscv64gc-unknown-linux-gnu,rustfmt-preview,clippy-preview)
	(cd $(@D)/wasm-std/rust-std-$(AOS_RUST_TOOLCHAIN_VERSION)-wasm32-unknown-unknown; \
		./install.sh \
			--prefix=/usr \
			--destdir=$(TARGET_DIR) \
			--disable-ldconfig \
			--components=rust-std-wasm32-unknown-unknown)
endef

$(eval $(generic-package))
