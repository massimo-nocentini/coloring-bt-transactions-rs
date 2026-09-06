# The article

`colouring.tex` — a detailed write-up of what the main binary computes, the
techniques it uses, and how to run it.

```sh
make            # pdflatex twice, for the table of contents
make latexmk    # or let latexmk work out the passes
make clean
```

## It has not been compiled

There is no TeX installation on the machine this was written on, so the source
has never been through a compiler. It is deliberately plain to make that as
low-risk as possible: the stock `article` class, and only packages that ship
with any TeX Live or MiKTeX (`amsmath`, `amssymb`, `booktabs`, `listings`,
`xcolor`, `graphicx`, `microtype`, `hyperref`, `geometry`). No custom class, no
bibliography, no `\write18`.

What *was* checked, mechanically: brace balance, `\begin`/`\end` pairing, that
every `\ref` has a `\label`, and that no `_` appears outside maths and listings.
That found one real bug — a `Section~\ref` pointing at an equation label — which
is fixed. It will not have found a missing package or a bad float placement, so
expect the first `make` to want a nudge.

## Figures

- `figures/ramp.png` — the `--palette` colour ramp, paper to ink
- `figures/chain-sampled.png` — the whole 2022 chain, 1024², one sampled
  transaction per cell
- `figures/chain-slice.png` — one million consecutive transactions, one pixel
  each, no binning

All three are generated, not drawn. The ramp comes from the `PLTE` chunk of a
`--palette` picture; the other two from `examples/tint.rs` over a whole-chain
`--moments` run.

## Where the numbers come from

Every figure in the article names the run that produced it. The corpora the
synthetic measurements use are written by `make corpus` in the repository root;
the chain measurements are against `finalBCUTXO_2022.scm` and are reproducible
with the commands quoted in the text. Claims that an adversarial review could
not confirm were left out rather than hedged — see the *Verification* section
for the two that were dropped and why.
