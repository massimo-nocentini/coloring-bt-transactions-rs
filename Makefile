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

# `asm-check` reads a binary that cargo has already built, so the two things it
# needs to be told are which disassembler to run and which architecture's
# mnemonics to expect.  Both default to this machine's and both are overridden
# by `cross-aarch64` below, which is the only way the aarch64 arm of the check
# gets exercised anywhere but on aarch64 hardware.  The cross default is LLVM's
# objdump, which is multi-target by construction; GNU binutils 2.42 built for
# x86-64 reads the file header, prints "file format elf64-little", and then
# gives up with "objdump: can't disassemble for architecture UNKNOWN!" on
# stderr, three lines on stdout and status 1.  Three lines is not a disassembly
# and the counts below would all be zero, so `asm-scan` looks at how much came
# back before it believes a zero.
OBJDUMP    ?= objdump
ARCH       ?= $(shell uname -m)

AARCH64_TARGET  := aarch64-unknown-linux-gnu
# Ubuntu ships llvm-objdump under its major version; plain `llvm-objdump` is
# what a Homebrew or a Fedora install calls it.  Override if neither is on PATH.
AARCH64_OBJDUMP ?= $(shell command -v llvm-objdump-18 >/dev/null 2>&1 \
                     && echo llvm-objdump-18 || echo llvm-objdump)
# cargo puts a cross build under a target triple directory, and honours
# CARGO_TARGET_DIR when it is set -- which it is whenever two builds must not
# share a lock.  A `build.target-dir` in a `.cargo/config.toml` would be the
# third place cargo looks and this does not read it; pass BIN= by hand there.
AARCH64_BIN     := $(or $(CARGO_TARGET_DIR),target)/$(AARCH64_TARGET)/release/$(CRATE)

.DEFAULT_GOAL := help

.PHONY: help
help: ## List the targets in this file
#	The digits in the class are for `cross-aarch64`; a name with a number in it
#	is otherwise silently absent from this listing rather than misprinted.
	@grep -E '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

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
asm-check: build ## Check the scaling and band loops still vectorise
#	Builds for this machine and hands the binary to `asm-scan`.  The two are
#	separate targets because `cross-aarch64` needs the scan without the build:
#	if the scan carried `build` as a prerequisite, cross-checking an aarch64
#	binary would first spend minutes producing an x86-64 one nobody looks at.
	@$(MAKE) --no-print-directory asm-scan

.PHONY: asm-scan
#	The check itself, over $(BIN), reading $(ARCH) for which mnemonics to expect
#	and $(OBJDUMP) for what to read them with.  Not in `help`: the two ways in
#	are `asm-check` for this machine and `cross-aarch64` for the other one.
#
#	`simd::scale_into` and `simd::scale_add_into` are plain loops that the
#	compiler is *expected* to vectorise -- nothing in the source says it must, so
#	nothing but the disassembly can confirm it did.  A refactor that quietly
#	stops it would cost speed silently, which is what this guards.  The same goes
#	for the elementwise loops in `bands`, which are the whole reason `--bands`
#	is faster than a term list.
#
#	The mnemonic to look for is the architecture's, so the pattern is picked by
#	$(ARCH) rather than tried everywhere: a grep that accepted both would pass
#	on aarch64 for an x86 reason and say nothing useful about either.
#
#	aarch64 -- two operand syntaxes, since LLVM's Mach-O output writes the lane
#	arrangement on the mnemonic (`fmul.2d v0, v1, v2`) and GNU's writes it on the
#	registers (`fmul v0.2d, v1.2d, v2.2d`).  Both mean two `f64` lanes, which is
#	all NEON has for doubles; `.4s` and `.2s` are the `f32` widths the band
#	store's own loops could reach.
#
#	The Mach-O half is the half that cannot be checked against a real binary from
#	here, so it was checked against the compiler instead: on 2026-09-07 the four
#	loop shapes were written into one small crate and emitted twice, `rustc -O
#	--emit asm` for `aarch64-apple-darwin` and for `aarch64-unknown-linux-gnu`.
#	Both patterns match their own syntax on that pair -- Mach-O 24 scale, 20 f32,
#	32 f64; ELF 12, 8, 16, the same loops with Apple's heavier default unroll.
#	Neither emitted a single `.2s` form, so that alternative is precaution and
#	not observation, and neither emitted `fmla` or `fmls`.
#
#	`fmla` is in the band patterns for the same reason and matches nothing today:
#	the full cross build of the same day has zero `fmla` and zero `fmls` in
#	203,593 disassembled lines, because LLVM will not contract `x*fa + y*fb`
#	without a fast-math flag this crate does not set.  It is there so that a
#	build which *does* fuse does not read as a build that stopped vectorising.
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
#	worst way for a guard to be wrong.  As of 2026-09-08 the x86-64 build has
#	eighteen of them and only eight are the scale loops: eight of the other ten
#	are the band store's own `scale` and `combine` -- three each in the `f64`
#	store's pair, one each in the `f32` store's, whose band lanes are `ps` and
#	whose moments are not -- and the last two are wherever the per-store `run`
#	ended up, which today is `main`: `run` is instantiated once per store, has one
#	caller, and is inlined into it rather than emitted under a name of its own.
#	So the ten sit in *five* symbols, not six, and the five are not a fact to lean
#	on -- which symbol swallows the last two is an inlining decision and moves
#	without warning.  That is exactly why the check is scoped by the `weighted`
#	and `bands` in the mangled name and never by how many symbols came back.  The
#	cross-built aarch64 binary of the same day and the same commit counts
#	identically, symbol for symbol: eighteen `fmul` at `.2d`, eight in `weighted`,
#	ten in those same five.  So de-vectorising both scale loops entirely still
#	leaves ten, and an unguarded count prints a healthy number and exits 0 --
#	re-measured by wrapping the two `*_into_uninit` loop bodies in
#	`core::hint::black_box`, which takes the `weighted` count from eight to zero
#	and leaves the other ten standing.  Re-run against the arm as it now stands on
#	2026-09-08: "scale loops: 0  [10 elsewhere]", "6 f32, 12 f64", exit 1.
#
#	Hence the enclosing symbol.  `objdump -d` writes `<symbol>:` above each
#	function, so awk carries the last one seen and only counts matches inside the
#	symbols the scale loops inline into -- the ones whose mangled name carries
#	this crate's `weighted` module, and the band loops' the ones carrying
#	`bands`.  A refactor that moves the call site elsewhere makes this fail rather
#	than pass, which is the right way round: a false failure gets looked at, a
#	false pass ships.
#
#	The band loops are checked in both precisions because that is where the two
#	`BandStore` instantiations differ: `--bands` with `f32` weights still carries
#	its moments in `f64`, so a build that vectorised only one of the two would be
#	half de-optimised and a single count would not show it.  The x86-64 arm has
#	required both since it was written; the aarch64 arm did not, and until
#	2026-09-07 a de-vectorised band loop passed silently on aarch64 -- the arm
#	checked the scale loops and nothing else.  Measured: with both band loop
#	bodies in `core::hint::black_box`, the aarch64 arm as it stood printed
#	"scale loops: 8" and exited 0, while the arm below prints "0 f32" and exits 1.
#
#	The `f32` half is the half that catches it, and that is not an accident of
#	this experiment -- it is a limit worth writing down.  `combine_into` and
#	`scale_into` also fold the three exact moments, always in `f64`, in the same
#	symbols; that little loop vectorises on its own and keeps a floor of `pd` and
#	`.2d` matches alive.  Re-run on 2026-09-08 against the arms as they now stand,
#	the same experiment leaves "0 f32, 6 f64" on x86-64 *and* on aarch64, and both
#	arms exit 1.  So the f64 count guards the `f32` store's moments and not the
#	`f64` store's lanes: if the lanes ever move out of these symbols, the count to
#	tighten is that one.
#
#	## The `next` that made the x86-64 f64 guard weaker than this comment said
#
#	Until 2026-09-08 the x86-64 arm could not count the `f64` band lanes at all,
#	and this note quoted 16 of them -- a figure no build could ever have printed.
#	The `mulpd` rule ended in `next`, so every `mulpd` line left the awk program
#	before either band rule was reached, and `bd` saw only the `addpd`: 4 of the
#	12 `pd` ops actually in the `bands` symbols, with all 8 vector multiplies
#	among the missing.  The `next` was skipping the width bookkeeping for a
#	non-`weighted` symbol, which a block around that bookkeeping does just as well
#	and without eating the line.  Scoping it that way changes nothing else,
#	measured the same day: the scale line still reads 8 at `%xmm` with 10
#	elsewhere, and the band line reads the same 12 the aarch64 arm had been
#	reading all along.
#
#	What the two arms print today, 2026-09-08, on this commit and a clean release
#	build -- `make asm-check` here, `make cross-aarch64` for the other:
#
#	  x86-64   vector f64 multiplies in the scale loops: 8  %xmm (2 lanes): 8
#	             [10 elsewhere in the binary, not counted]
#	           vector multiplies and adds in the band loops: 6 f32, 12 f64
#	  aarch64  vector f64 multiplies in the scale loops: 8 (2 lanes each)
#	             [10 elsewhere in the binary, not counted]
#	           vector multiplies and adds in the band loops: 6 f32, 12 f64
#
#	The two agreeing is not something to assert.  The band counts are a function
#	of how far the compiler unrolled and where it inlined, the x86-64 figure in
#	this note has already been wrong once for a reason with nothing to do with the
#	loops it describes, and quoting them is a record of one build and not a
#	contract -- which is why the check is "non-zero" and not "equal to".
#
#	The register cannot be found by scanning forward from the mnemonic, which is
#	the obvious thing and is wrong: an AVX memory operand is `(%rdx,%rdi,8)` and
#	carries commas of its own, so any "up to the first comma" pattern stops
#	inside the addressing mode and never reaches the register.  Hence awk over
#	the whole line, widest register wins.
asm-scan:
	@test -f $(BIN) || { echo "asm-scan: no binary at $(BIN)"; exit 1; }; \
	lines=$$($(OBJDUMP) -d $(BIN) 2>/dev/null | wc -l); \
	test "$$lines" -gt 100 || { \
	  echo "asm-scan: $(OBJDUMP) gave $$lines lines for $(BIN), which is not a disassembly -- it cannot read that architecture; pass OBJDUMP=<a multi-target objdump, llvm-objdump for one>"; \
	  exit 1; }; \
	case "$(ARCH)" in \
	  aarch64|arm64) \
	    $(OBJDUMP) -d $(BIN) | awk ' \
	      /^[0-9a-f]+ <.*>:/ { sym = $$2 } \
	      /(^|[ \t])(fmul\.2d|fmul[ \t]+v[0-9]+\.2d)/ { \
	        n++; if (sym ~ /weighted/) w++ } \
	      /(^|[ \t])(f(mul|mla|add)\.(2s|4s)|f(mul|mla|add)[ \t]+v[0-9]+\.(2s|4s))/ \
	        && sym ~ /bands/ { bs++ } \
	      /(^|[ \t])(f(mul|mla|add)\.2d|f(mul|mla|add)[ \t]+v[0-9]+\.2d)/ \
	        && sym ~ /bands/ { bd++ } \
	      END { printf "vector f64 multiplies in the scale loops: %d (2 lanes each)", w+0; \
	            printf "  [%d elsewhere in the binary, not counted]\n", n-w; \
	            printf "vector multiplies and adds in the band loops: %d f32, %d f64\n", bs+0, bd+0; \
	            exit (w+0) == 0 || (bs+0) == 0 || (bd+0) == 0 } ';; \
	  x86_64|amd64) \
	    $(OBJDUMP) -d $(BIN) | awk ' \
	      /^[0-9a-f]+ <.*>:/ { sym = $$2 } \
	      /(^|[ \t])(v?mulpd|vfmadd[0-9]*pd)[ \t]/ { \
	        n++; \
	        if (sym ~ /weighted/) { \
	          w++; \
	          if (/%zmm/) z++; else if (/%ymm/) y++; else if (/%xmm/) x++ } } \
	      /(^|[ \t])(v?(mul|add)ps|vfmadd[0-9]*ps)[ \t]/ && sym ~ /bands/ { bs++ } \
	      /(^|[ \t])(v?(mul|add)pd|vfmadd[0-9]*pd)[ \t]/ && sym ~ /bands/ { bd++ } \
	      END { printf "vector f64 multiplies in the scale loops: %d", w+0; \
	            if (x) printf "  %%xmm (2 lanes): %d", x; \
	            if (y) printf "  %%ymm (4 lanes): %d", y; \
	            if (z) printf "  %%zmm (8 lanes): %d", z; \
	            printf "  [%d elsewhere in the binary, not counted]\n", n-w; \
	            printf "vector multiplies and adds in the band loops: %d f32, %d f64\n", bs+0, bd+0; \
	            exit (w+0) == 0 || (bs+0) == 0 || (bd+0) == 0 } ';; \
	  *) \
	    echo "asm-scan: no pattern for $(ARCH), skipping"; exit 0;; \
	esac || { \
	  echo "FAIL: a loop that should be vectorised is running one lane at a time -- see the counts above"; exit 1; }

.PHONY: cross-aarch64
cross-aarch64: ## Cross-build for aarch64 and run asm-check against that binary
#	The aarch64 arm of `asm-scan` above cannot be exercised on an x86-64 machine
#	without an aarch64 binary to read, and until 2026-09-07 nothing here could
#	produce one -- so that arm shipped for however long unrun, which is how it
#	came to check half of what the x86-64 arm checks.  This target is the answer:
#	it builds the crate for $(AARCH64_TARGET) and runs the scan over the result
#	with $(AARCH64_OBJDUMP), which reads any architecture.  GNU `objdump` on an
#	x86-64 host does not -- see the note over `asm-scan` for exactly how it fails
#	-- which is why $(OBJDUMP) is a variable at all.
#
#	What it needs, and how to get it without root.  `rustup target add
#	aarch64-unknown-linux-gnu` supplies the Rust side; the C side is a linker, a
#	cross libc and a cross libgcc, because `tikv-jemalloc-sys` compiles and links
#	C.  On a Debian or Ubuntu host with sudo that is
#	`apt install gcc-aarch64-linux-gnu qemu-user-static` and nothing else to do.
#	Without sudo the same packages -- `libc6-dev-arm64-cross`,
#	`libgcc-s1-arm64-cross`, `gcc-13-cross-base`, `qemu-user-static` -- come down
#	with `apt-get download` and unpack with `dpkg-deb -x` into any directory, and
#	clang is told about them by hand:
#
#	  CC_aarch64_unknown_linux_gnu=clang
#	  CFLAGS_aarch64_unknown_linux_gnu='--target=aarch64-unknown-linux-gnu \
#	      --sysroot=$$SR -B$$GCCD -L$$GCCD -fuse-ld=lld -Qunused-arguments'
#	  AR_aarch64_unknown_linux_gnu=llvm-ar
#	  CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=clang
#	  CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-Clink-arg=... (the same)'
#	  CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER='qemu-aarch64-static -L $$SR'
#
#	The flags have to be in CFLAGS as well as in the rustc link args because
#	jemalloc runs autotools `configure`, whose link test uses $$CC $$CFLAGS alone
#	and otherwise fails with "C compiler cannot create executables".  Two symlinks
#	are wanted inside the unpacked sysroot: `libgcc_s.so` -> `libgcc_s.so.1`,
#	which the cross package omits, and `usr/aarch64-linux-gnu/lib` -> `lib`,
#	because glibc's `libc.so` and `libm.so` are linker scripts naming that
#	absolute path, resolved inside `--sysroot`.
#
#	With RUNNER set, `cargo test --target $(AARCH64_TARGET)` runs the whole suite
#	under qemu-user on this machine.  That is worth doing and is not what this
#	target does: qemu tells you the answers are the same, the disassembly is the
#	only thing that tells you the loops are still wide, and qemu models no
#	microarchitecture at all, so neither says anything about NEON throughput on
#	real silicon.
#
#	## What this has actually been run to, and what it still does not cover
#
#	2026-09-07, this target on an x86-64 Ubuntu box with the sysroot above and
#	llvm-objdump 18.1.3: cross-build clean, and the scan reads
#	"8 (2 lanes each) [10 elsewhere]" and "6 f32, 12 f64" off the aarch64 binary.
#	`cargo test --target $(AARCH64_TARGET)` on the same tree runs 305 tests under
#	qemu, 0 failed and 1 ignored, exactly what the host runs and exactly what the
#	host passes.  And eight folds over real prefixes of `finalBCUTXO_2022.scm`
#	agree byte for byte between the two architectures, every byte of every line:
#
#	  50,000 records   --weighted            1,243,359 B    0.26 s
#	  50,000 records   --sets                  848,583 B    0.20 s
#	 300,000 records   --weighted --sum      5,314,833 B   41.58 s
#	 300,000 records   --weighted --moments 10,811,232 B   61.20 s
#	 300,000 records   --bands 1024 --mom.  10,881,276 B    7.74 s
#	 300,000 records   --bands64 1024 --m.  10,842,192 B    8.39 s
#	2,000,000 records  --weighted --sum     43,054,523 B 2702.68 s
#	2,000,000 records  --weighted --moments 90,754,069 B 3664.92 s
#
#	The times are qemu's and are the reason for the next paragraph rather than a
#	measurement of anything: the same two-million-record `--sum` is 280.84 s
#	native on this box, so qemu is running it 9.6 times slower, and that ratio is
#	a property of the emulator.
#
#	None of this is a statement about speed.  qemu-user translates instructions
#	and models no pipeline, no cache and no issue width, so a NEON kernel that is
#	correct here may still be slower than the scalar loop it replaced on real
#	silicon; only a machine says otherwise, and there is no aarch64 machine here.
#	And the Mach-O half of the patterns in `asm-scan` -- `fmul.2d`, `fadd.4s` --
#	is matched against assembly the compiler emitted for `aarch64-apple-darwin`
#	and has never been matched against a linked Mach-O binary, because nothing
#	here can link one: `cargo build --target aarch64-apple-darwin` gets as far as
#	the C dependencies and stops at `cc: error: unrecognized command-line option
#	'-arch'`.  Compiler output and linker output are not the same artefact, and
#	only the first of the two has been seen.
	@rustup target list --installed 2>/dev/null | grep -qx '$(AARCH64_TARGET)' || { \
	  echo "cross-aarch64: $(AARCH64_TARGET) is not installed; run 'rustup target add $(AARCH64_TARGET)'"; exit 1; }
	@command -v $(AARCH64_OBJDUMP) >/dev/null 2>&1 || { \
	  echo "cross-aarch64: no $(AARCH64_OBJDUMP) on PATH, and GNU objdump built for this host cannot read aarch64; install LLVM's or pass AARCH64_OBJDUMP=<path>"; exit 1; }
	cargo build --release --target $(AARCH64_TARGET)
	@$(MAKE) --no-print-directory asm-scan \
		ARCH=aarch64 BIN=$(AARCH64_BIN) OBJDUMP=$(AARCH64_OBJDUMP)

.PHONY: test
test: ## Run the test suite
	cargo test

.PHONY: clean
clean: ## Remove cargo's build output and the published docs
	cargo clean
	rm -rf $(DOCS)
