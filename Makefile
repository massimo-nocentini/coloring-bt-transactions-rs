# Tasks for coloring-bt-transactions.  `cargo` does the work; this file exists
# for the one job cargo has no rule for -- getting rustdoc's output into `docs/`
# in a shape GitHub Pages will serve.

CRATE      := coloring-bt-transactions
# rustdoc turns the hyphens in a crate name into underscores for the directory
# it writes, so the two names are not interchangeable below.
CRATE_DIR  := $(subst -,_,$(CRATE))
DOC_OUT    := target/doc
DOCS       := docs

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

.PHONY: test
test: ## Run the test suite
	cargo test

.PHONY: clean
clean: ## Remove cargo's build output and the published docs
	cargo clean
	rm -rf $(DOCS)
