# vipc_test 依赖已废弃的外部 vqueue/libvqueue 构建链，不纳入 StarryOS 默认回归。
# 需要验证 IPC 时，应在独立 vipc 工程中构建对应测试。
UAPPS := $(filter-out config.toml target Makefile README.md vipc_test, $(notdir $(wildcard ./*)))

# build_uapps: $(UAPPS)
# 	@$(foreach uapp, $(UAPPS), cd $(CURDIR)/$(uapp) && cargo build --release && cd ..;)

build_uapps:
	@for uapp in $(UAPPS); do \
		echo "==== Building $$uapp ===="; \
# 		(cd $(CURDIR)/$$uapp && rm -f Cargo.lock && cargo build --release) || exit $$?; \
		(cd $(CURDIR)/$$uapp && cargo build --release) || exit $$?; \
	done
