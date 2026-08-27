# Tasks for coloring-bt-transactions.  `cargo` does the work; this file exists
# for the one job cargo has no rule for -- getting rustdoc's output into `docs/`
# in a shape GitHub Pages will serve.

CRATE      := coloring-bt-transactions
# rustdoc turns the hyphens in a crate name into underscores for the directory
# it writes, so the two names are not interchangeable below.
CRATE_DIR  := $(subst -,_,$(CRATE))
DOC_OUT    := target/doc
DOCS       := docs
BIN        := target/release/$(CRATE)

.DEFAULT_GOAL := help

.PHONY: help
help: ## List the targets in this file
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

.PHONY: docs
docs: ## Build the API docs and publish them to docs/
#	rustdoc's search index lives in chunks whose names are hashes of their
#	contents, and it writes the new ones without removing the ones they replace,
#	so an incremental doc build leaves junk behind -- 117 files where a clean one
#	produces 112.  Since docs/ is checked in, that junk would show up as a
#	handful of renamed files in every commit that rebuilds the docs.  rustdoc
#	itself is deterministic: build it from nothing and the output is a function
#	of the source alone.  The crate is small enough that this costs under a
#	second.
	rm -rf $(DOC_OUT)
	cargo doc --no-deps
#	The directory is entirely generated, so it is replaced rather than merged:
#	copying over the top would leave behind pages for items that no longer
#	exist, and those are worse than missing ones because they look current.
	rm -rf $(DOCS)
	mkdir -p $(DOCS)
	cp -R $(DOC_OUT)/. $(DOCS)/
#	cargo keeps a lock file in its output directory.  It is build machinery, not
#	documentation, and `cp -R` of a dotted source picks it up.
	rm -f $(DOCS)/.lock
#	Pages runs Jekyll by default, which drops files and directories whose names
#	begin with an underscore.  rustdoc emits several.  This switches it off.
	touch $(DOCS)/.nojekyll
#	rustdoc writes no index.html at the root of its output -- the landing page
#	is one level down, under the crate's own directory -- so serving docs/
#	directly would show a file listing.  This is the redirect cargo itself uses
#	when it has a single crate to point at.
	printf '<meta http-equiv="refresh" content="0; url=%s/index.html">\n' \
		'$(CRATE_DIR)' > $(DOCS)/index.html
	@echo "docs published to $(DOCS)/ -- entry point $(DOCS)/$(CRATE_DIR)/index.html"

.PHONY: docs-strict
docs-strict: ## Build the docs, treating any rustdoc warning as an error
	RUSTDOCFLAGS='-D warnings' $(MAKE) docs

.PHONY: docs-open
docs-open: docs ## Publish the docs and open them in a browser
#	Opens cargo's own copy rather than docs/, which is the same content, because
#	cargo knows how to open a browser on every platform and this file does not.
	cargo doc --no-deps --open

.PHONY: build
build: ## Release build
	cargo build --release

.PHONY: view
view: ## Open GRAPH=<basename> in the windowed viewer
#	`tree-view` is the one target here that needs something installed beyond a
#	Rust toolchain: GTK 4 and its development headers, which the `gui` feature
#	exists to keep out of every other build.  On Debian and Ubuntu that is
#	`libgtk-4-dev`; on Fedora `gtk4-devel`; on macOS `brew install gtk4`.
	@test -n "$(GRAPH)" || { echo "usage: make view GRAPH=<graph-basename>"; exit 1; }
	cargo run --release --features gui --bin tree-view -- $(GRAPH)

.PHONY: pdf
pdf: ## Draw RECORDS=<file> as one page in PDF=<file>
#	`--pdf` draws with Cairo, so it wants a `libcairo` and its headers on the
#	machine -- and nothing else: no toolkit, unlike `view` and `tx-view` below.
#	On Debian and Ubuntu that is `libcairo2-dev`; on Fedora `cairo-devel`; on
#	macOS `brew install cairo`.
#
#	`--sets` because the page drops coefficients anyway and the sorted arrays
#	are several times faster than the rings at producing the same set of blocks.
	@test -n "$(RECORDS)" || { echo "usage: make pdf RECORDS=<file> [PDF=<file>] [PAGE=<n>]"; exit 1; }
	cargo run --release --features pdf --bin $(CRATE) -- all --sets \
		--pdf $(or $(PDF),out.pdf) $(if $(PAGE),--page $(PAGE),) < $(RECORDS)

.PHONY: picture
picture: ## Show RECORDS=<file> as the picture in a window
#	The same drawing `pdf` writes, in a window one can move and zoom instead --
#	so this wants GTK as well as Cairo, exactly as `view` and `tx-view` do; see
#	the note under `view`.
#
#	`PAGE` is worth raising here in a way it is not for a page: the window zooms,
#	and the cells one can climb into are the ones the fold put there.
	@test -n "$(RECORDS)" || { echo "usage: make picture RECORDS=<records-file> [PAGE=<n>]"; exit 1; }
	cargo run --release --features gui --bin $(CRATE) -- all --sets \
		--view $(if $(PAGE),--page $(PAGE),) < $(RECORDS)

.PHONY: tx-view
tx-view: ## Open RECORDS=<file> in the transaction viewer
#	The same window as `view`, over a file of transaction records rather than a
#	webgraph: each one drawn under the transaction its first input spends, and
#	coloured by the blocks its coins came from.  Needs GTK 4 exactly as `view`
#	does -- see the note there.
	@test -n "$(RECORDS)" || { echo "usage: make tx-view RECORDS=<records-file>"; exit 1; }
	cargo run --release --features gui --bin tx-view -- $(RECORDS)

.PHONY: test-pdf
test-pdf: ## Run the test suite including the page's, which needs Cairo
#	`page`'s tests fold pictures onto canvases and write a page to a temporary
#	file, so they want Cairo; `make test` runs everything else with no C library
#	on the machine at all.
	cargo test --features pdf

.PHONY: test-gui
test-gui: ## Run the test suite including the windows', which needs GTK
#	The windows' own tests -- the viewers' and `--view`'s -- draw frames onto an
#	image surface and look at the pixels, so they want Cairo but never a screen;
#	`make test` runs everything else, including the camera and the quadtree, with
#	no toolkit at all.  `gui` includes `pdf`, so this is `test-pdf` and then some.
	cargo test --features gui

.PHONY: asm-check
asm-check: build ## Check the weight-scaling loops still vectorise
#	`simd::scale_into` and `simd::scale_add_into` are plain loops that the
#	compiler is *expected* to vectorise -- nothing in the source says it must, so
#	nothing but the disassembly can confirm it did.  A refactor that quietly
#	stops it would cost speed silently, which is what this guards.
#
#	Two operand syntaxes are accepted: LLVM's Mach-O output writes the lane
#	arrangement on the mnemonic (`fmul.2d v0, v1, v2`), GNU's writes it on the
#	registers (`fmul v0.2d, v1.2d, v2.2d`).
	@count=$$(objdump -d $(BIN) \
		| grep -cE '(\bfmul\.2d\b|\bfmul[[:space:]]+v[0-9]+\.2d)' || true); \
	echo "vector f64 multiplies (2 lanes each): $$count"; \
	if [ "$$count" -eq 0 ]; then \
		echo "FAIL: the weight scaling is running one lane at a time"; exit 1; \
	fi

.PHONY: test
test: ## Run the test suite
	cargo test

.PHONY: clean
clean: ## Remove cargo's build output and the published docs
	cargo clean
	rm -rf $(DOCS)
