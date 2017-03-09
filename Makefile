SUBMODULES = testdata/Packages/.git

info:
	$(info Targets)
	$(info -----------------------------------------------------------------------)
	$(info assets      | generate default theme packs and syntax)
	$(info - OTHER TARGETS -------------------------------------------------------)
	$(info themes      | generate default theme pack)
	$(info packs       | generate default syntax pack)
	
	
$(SUBMODULES):
	git submodule update --init --recursive

assets: packs themes

packs: $(SUBMODULES)
	cargo run --example gendata -- synpack testdata/Packages assets/default_newlines.packdump assets/default_nonewlines.packdump 
	
themes: $(SUBMODULES)
	cargo run --example gendata -- themepack testdata assets/default.themedump
	