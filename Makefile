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

.PHONY: subtree
subtree: ## Draw ROOT=<id>'s subtree of GRAPH=<basename> into PDF=<file>
#	`tree-pdf` writes its page with no C library at all -- the PDF writer is in
#	the crate -- so unlike `pdf` below this builds anywhere `cargo` does.  The
#	cut is the caller's: pass ARGS='--depth 12 --fanout 16 --vertical' and so
#	on; run the binary with -h for the full set.
	@test -n "$(GRAPH)" && test -n "$(ROOT)" || { \
		echo "usage: make subtree GRAPH=<graph-basename> ROOT=<id>[,<id>...] [PDF=<file>] [ARGS='--depth 12 ...']"; exit 1; }
	cargo run --release --bin tree-pdf -- $(GRAPH) --root $(ROOT) $(ARGS) \
		-o $(or $(PDF),subtree.pdf)

.PHONY: blocks
blocks: ## Draw the blocks around ROOT=<id> of GRAPH=<basename>, TRANSPOSE=<basename>, into PDF=<file>
#	`block-pdf` is `subtree` with the block as its unit: every transaction it
#	admits drawn as a complete bipartite gadget, inputs over outputs, all its
#	arcs.  The transpose is what finds a block's inputs, so it is not optional.
#	Same writer as `tree-pdf`, so no C library; the cut is the caller's --
#	pass ARGS='--depth 2 --labels' and so on, or run the binary with -h.
	@test -n "$(GRAPH)" && test -n "$(TRANSPOSE)" && test -n "$(ROOT)" || { \
		echo "usage: make blocks GRAPH=<graph-basename> TRANSPOSE=<transpose-basename> ROOT=<id>[,<id>...] [PDF=<file>] [ARGS='--depth 2 ...']"; exit 1; }
	cargo run --release --bin block-pdf -- $(GRAPH) $(TRANSPOSE) --root $(ROOT) $(ARGS) \
		-o $(or $(PDF),blocks.pdf)

.PHONY: pdf
pdf: ## Draw RECORDS=<file> as one page in PDF=<file>
#	`--pdf` draws with Cairo, so it wants a `libcairo` and its headers on the
#	machine -- and nothing else: no toolkit, unlike `view` and `tx-view` below.
#	On Debian and Ubuntu that is `libcairo2-dev`; on Fedora `cairo-devel`; on
#	macOS `brew install cairo`.
#
#	`--sets` because an unweighted cell counts the blocks it covers and the sorted
#	arrays are several times faster than the rings at producing the same set of
#	them.  Add `--weighted` for a page shaded by how much of each transaction's
#	value came through the blocks rather than by how many of them it reached.
	@test -n "$(RECORDS)" || { echo "usage: make pdf RECORDS=<file> [PDF=<file>]"; exit 1; }
	cargo run --release --features pdf --bin $(CRATE) -- all --sets \
		--pdf $(or $(PDF),out.pdf) < $(RECORDS)

.PHONY: picture
picture: ## Show RECORDS=<file> as the picture in a window
#	The same drawing `pdf` writes, in a window one can move and zoom instead --
#	so this wants GTK as well as Cairo, exactly as `view` and `tx-view` do; see
#	the note under `view`.
#
	@test -n "$(RECORDS)" || { echo "usage: make picture RECORDS=<records-file>"; exit 1; }
	cargo run --release --features gui --bin $(CRATE) -- all --sets \
		--view < $(RECORDS)

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

.PHONY: corpus
corpus: ## Write the two corpora the measurements in the docs were taken over
#	Every performance number in `emit`, `prefetch` and the driver's own docs
#	names one of these two files.  Without them those numbers are assertions
#	nobody can check, which is what this target is for -- the generator is
#	deterministic, so a given seed is one exact file on every machine.
#
#	`records` is the shape a real chain has: a transaction reaches back across
#	some hundreds of earlier ones, so ancestry mixes and colours grow to a few
#	thousand blocks.  That is the regime where formatting a line is most of the
#	run, and so the regime `--threads` exists for.
#
#	`flat` is the other end: every spend stays inside its own block, so every
#	colour is one block and a line is ten bytes.  Nothing about the fold is
#	interesting there -- what it measures is the pipeline's own overhead, which
#	is what chose the batch bounds in `emit`.
#
#	About 180 MB the pair, and under a second to write.
	cargo run --release --example records -- --window 4000 > $(or $(RECORDS),records)
	cargo run --release --example records -- --window 0    > $(or $(FLAT),flat)

.PHONY: asm-check
asm-check: build ## Check the weight-scaling loops still vectorise
#	`simd::scale_into` and `simd::scale_add_into` are plain loops that the
#	compiler is *expected* to vectorise -- nothing in the source says it must, so
#	nothing but the disassembly can confirm it did.  A refactor that quietly
#	stops it would cost speed silently, which is what this guards.
#
#	The mnemonic to look for is the architecture's, so the pattern is picked by
#	`uname -m` rather than tried everywhere: a grep that accepted both would pass
#	on aarch64 for an x86 reason and say nothing useful about either.
#
#	aarch64 -- two operand syntaxes, since LLVM's Mach-O output writes the lane
#	arrangement on the mnemonic (`fmul.2d v0, v1, v2`) and GNU's writes it on the
#	registers (`fmul v0.2d, v1.2d, v2.2d`).  Both mean two `f64` lanes, which is
#	all NEON has.
#
#	x86-64 -- the lane count is not fixed the way NEON's is, it is a function of
#	what the build was told to target, so the width is read off the register the
#	instruction names and reported rather than assumed.  A default `x86-64` build
#	is SSE2 and gets `mulpd %xmm` at two lanes; `-C target-cpu=native` reaches
#	`vmulpd`/`vfmadd...pd` on `%ymm` at four, or `%zmm` at eight where LLVM
#	judges the downclocking worth it.  The width is worth watching -- the default
#	target leaves half the machine's lanes unused -- but it is not what fails the
#	check, which is only ever about the loops having vectorised.
#
#	## Only the loops this is about
#
#	Counting vector multiplies across the whole binary fails *open*, which is the
#	worst way for a guard to be wrong.  Today's build has seventeen of them and
#	only ten are the scale loops: six are in `main` and one is in a closure in
#	`emit`.  So de-vectorising both loops entirely still left seven, and the
#	target printed a healthy number and exited 0 -- verified by wrapping the two
#	loop bodies in `core::hint::black_box`, which took the count inside
#	`WeightedSets::combine` from ten to zero and passed anyway.
#
#	Hence the enclosing symbol.  `objdump -d` writes `<symbol>:` above each
#	function, so awk carries the last one seen and only counts matches inside the
#	symbols the scale loops inline into -- the ones whose mangled name carries
#	this crate's `weighted` module.  A refactor that moves the call site
#	elsewhere makes this fail rather than pass, which is the right way round: a
#	false failure gets looked at, a false pass ships.
#
#	The register cannot be found by scanning forward from the mnemonic, which is
#	the obvious thing and is wrong: an AVX memory operand is `(%rdx,%rdi,8)` and
#	carries commas of its own, so any "up to the first comma" pattern stops
#	inside the addressing mode and never reaches the register.  Hence awk over
#	the whole line, widest register wins.
	@arch=$$(uname -m); \
	case "$$arch" in \
	  aarch64|arm64) \
	    objdump -d $(BIN) | awk ' \
	      /^[0-9a-f]+ <.*>:/ { sym = $$2 } \
	      /(^|[ \t])(fmul\.2d|fmul[ \t]+v[0-9]+\.2d)/ { \
	        n++; if (sym ~ /weighted/) w++ } \
	      END { printf "vector f64 multiplies in the scale loops: %d (2 lanes each)", w+0; \
	            printf "  [%d elsewhere in the binary, not counted]\n", n-w; \
	            exit (w+0) == 0 } ';; \
	  x86_64|amd64) \
	    objdump -d $(BIN) | awk ' \
	      /^[0-9a-f]+ <.*>:/ { sym = $$2 } \
	      /(^|[ \t])(v?mulpd|vfmadd[0-9]*pd)[ \t]/ { \
	        n++; \
	        if (sym !~ /weighted/) next; \
	        w++; \
	        if (/%zmm/) z++; else if (/%ymm/) y++; else if (/%xmm/) x++ } \
	      END { printf "vector f64 multiplies in the scale loops: %d", w+0; \
	            if (x) printf "  %%xmm (2 lanes): %d", x; \
	            if (y) printf "  %%ymm (4 lanes): %d", y; \
	            if (z) printf "  %%zmm (8 lanes): %d", z; \
	            printf "  [%d elsewhere in the binary, not counted]\n", n-w; \
	            exit (w+0) == 0 } ';; \
	  *) \
	    echo "asm-check: no pattern for $$arch, skipping"; exit 0;; \
	esac || { \
	  echo "FAIL: the weight scaling is running one lane at a time"; exit 1; }

.PHONY: test
test: ## Run the test suite
	cargo test

.PHONY: clean
clean: ## Remove cargo's build output and the published docs
	cargo clean
	rm -rf $(DOCS)
