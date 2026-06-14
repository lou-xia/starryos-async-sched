UAPPS := $(filter-out config.toml target Makefile README.md, $(notdir $(wildcard ./*)))

# build_uapps: $(UAPPS)
# 	@$(foreach uapp, $(UAPPS), cd $(CURDIR)/$(uapp) && cargo build --release && cd ..;)

build_uapps:
	@for uapp in $(UAPPS); do \
		echo "==== Building $$uapp ===="; \
# 		(cd $(CURDIR)/$$uapp && rm -f Cargo.lock && cargo build --release) || exit $$?; \
		(cd $(CURDIR)/$$uapp && cargo build --release) || exit $$?; \
	done