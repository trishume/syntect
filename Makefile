SUBMODULES = testdata/Packages/.git

info:
	$(info Targets)
	$(info -----------------------------------------------------------------------)
	$(info assets      | generate default theme packs and syntax)
	$(info - OTHER TARGETS -------------------------------------------------------)
	$(info themes      | generate default theme pack)
	$(info packs       | generate default syntax pack)
	$(info syntest     | run syntax test summary)


$(SUBMODULES):
	git submodule update --init --recursive

assets: packs themes

packs: $(SUBMODULES)
	cargo run --features=metadata --example gendata -- synpack testdata/Packages assets/default_newlines.packdump assets/default_nonewlines.packdump assets/default_metadata.packdump testdata/DefaultPackage

themes: $(SUBMODULES)
	cargo run --example gendata -- themepack testdata assets/default.themedump

syntest: $(SUBMODULES)
	@echo Tip: Run make update-known-failures to update the known failures file.
	cargo run --release --example syntest -- testdata/Packages testdata/Packages --summary | diff -U 1000000 testdata/known_syntest_failures.txt -
	@echo No new failures!

update-known-failures: $(SUBMODULES)
	cargo run --release --example syntest -- testdata/Packages testdata/Packages --summary | tee testdata/known_syntest_failures.txt

