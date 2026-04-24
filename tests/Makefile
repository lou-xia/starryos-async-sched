UAPPS := $(filter-out config.toml target Makefile README.md, $(notdir $(wildcard ./*)))

build_uapps: $(UAPPS)
	@$(foreach uapp, $(UAPPS), cd $(CURDIR)/$(uapp) && cargo build --release && cd ..;)
