# Build Options
export ARCH := riscv64
export LOG := warn
export DWARF := y
export MEMTRACK := n

# vsched2 uses the mutable build-time constant CPU_NUM, while ArceOS exposes
# the same setting as the make variable SMP.  Keep the kernel crate, generated
# vDSO, and their per-CPU arrays on one value whenever SMP is explicitly set.
ifneq ($(SMP),)
export CPU_NUM := $(SMP)
endif

# QEMU Options
export BLK := y
export NET := y
export VSOCK := n
export MEM := 1G
export ICOUNT := n

# Generated Options
export A := $(PWD)
export NO_AXSTD := y
export AX_LIB := axfeat
export APP_FEATURES := qemu

ifeq ($(MEMTRACK), y)
	APP_FEATURES += starry-api/memtrack
endif

default: build

all: build
	cp $(PWD)/StarryOS_riscv64-qemu-virt.elf $(PWD)/kernel-rv
	touch $(PWD)/kernel-la

ROOTFS_URL = https://github.com/Starry-OS/rootfs/releases/download/20260214
ROOTFS_IMG = rootfs-$(ARCH).img

rootfs:
	@if [ ! -f $(ROOTFS_IMG) ]; then \
		echo "Image not found, downloading..."; \
		curl -f -L $(ROOTFS_URL)/$(ROOTFS_IMG).xz -O; \
		xz -d $(ROOTFS_IMG).xz; \
	fi
	@cp $(ROOTFS_IMG) arceos/disk.img

copy_tests: rootfs
	@if [ -d tests ]; then \
		set -e; \
		echo "Copying tests folder to disk.img..."; \
		mkdir -p /tmp/disk_mount; \
		sudo mount -t ext4 -o loop arceos/disk.img /tmp/disk_mount; \
		trap 'sudo umount /tmp/disk_mount' EXIT; \
		sudo rm -rf /tmp/disk_mount/tests; \
		sudo mkdir -p /tmp/disk_mount/tests/target/riscv64gc-unknown-linux-musl/release; \
		find tests/target/riscv64gc-unknown-linux-musl/release \
			-maxdepth 1 -type f -perm /111 \
			-exec sudo cp {} /tmp/disk_mount/tests/target/riscv64gc-unknown-linux-musl/release/ \;; \
		sudo umount /tmp/disk_mount; \
		trap - EXIT; \
		rm -rf /tmp/disk_mount; \
		echo "Tests copied successfully."; \
	fi

img:
	@echo -e "\033[33mWARN: The 'img' target is deprecated. Please use 'rootfs' instead.\033[0m"
	@$(MAKE) --no-print-directory rootfs

defconfig justrun clean:
	@make -C arceos $@

build run debug disasm: defconfig
	@rm -f .axconfig.toml
	@make -C arceos $@

uapp:
	cd tests && $(MAKE) build_uapps

test: uapp copy_tests
	@rm -f .axconfig.toml
	@make -C arceos run

verify-vsched2: build
	@bash scripts/check-vsched2-log.sh

# Aliases
rv:
	$(MAKE) ARCH=riscv64 run

la:
	$(MAKE) ARCH=loongarch64 run

vf2:
	$(MAKE) ARCH=riscv64 APP_FEATURES=vf2 MYPLAT=axplat-riscv64-visionfive2 BUS=mmio build

.PHONY: build run justrun debug disasm clean verify-vsched2
