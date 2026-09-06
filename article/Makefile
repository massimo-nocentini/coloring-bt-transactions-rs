# The article.  `make` builds the PDF; `make clean` removes everything but the
# source and the figures.
#
# There is no LaTeX on the machine this was written on, so the source has never
# been through a compiler here -- see README.md.  It is deliberately plain: the
# `article` class and packages that ship with any TeX Live or MiKTeX install
# (amsmath, booktabs, listings, xcolor, graphicx, microtype, hyperref,
# geometry), no custom classes, no bibliography, no shell-escape.

TEX  := colouring.tex
PDF  := $(TEX:.tex=.pdf)
AUX  := $(TEX:.tex=.aux) $(TEX:.tex=.log) $(TEX:.tex=.out) $(TEX:.tex=.toc)

.DEFAULT_GOAL := $(PDF)

# Twice, because the table of contents needs the second pass to be right.
# latexmk would do this for us where it exists; this keeps the dependency list
# to pdflatex alone.
$(PDF): $(TEX) $(wildcard figures/*.png)
	pdflatex -interaction=nonstopmode -halt-on-error $(TEX)
	pdflatex -interaction=nonstopmode -halt-on-error $(TEX)

.PHONY: latexmk
latexmk: ## Build with latexmk instead, which works out the passes itself
	latexmk -pdf -interaction=nonstopmode $(TEX)

.PHONY: clean
clean:
	rm -f $(AUX) $(PDF) $(TEX:.tex=.fls) $(TEX:.tex=.fdb_latexmk)
