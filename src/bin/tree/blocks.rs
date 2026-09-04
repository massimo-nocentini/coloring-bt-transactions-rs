//! # A webgraph, read as blocks
//!
//! The part of a block drawing that is about the *graph* rather than about the
//! ink.  [`forest`](super) reads a graph as a spanning tree, one parent per
//! node; this reads it as the theorem of the payments graph says it is made:
//! a set of complete bipartite **blocks** K(I, O), one per transaction, whose
//! inputs I are the outputs it spent and whose outputs O are the ones it
//! created, with every one of the |I|·|O| arcs of I × O in the graph and no
//! other arc into O.  A node is an output of at most one block and an input of
//! at most one, so the blocks chain through the nodes they share, and the
//! *quotient* — blocks as nodes, "an output of b is an input of c" as arcs —
//! is a DAG the same breadth-first walk can spread over a page.
//!
//! # What a spanning tree hides, and this does not
//!
//! In a spanning tree of the graph exactly one arc per non-source node
//! survives, so a block with two inputs is drawn with one input feeding all
//! its outputs and the other a bare leaf — its arcs "dropped" as second
//! parents.  Over the whole graph three arcs in four go that way.  Here a
//! block is drawn whole or not at all: every input on a row, every output on
//! the row below, every arc between them.  Nothing is ever dropped, and the
//! report says `dropped=0` on every run so that a caption can say so.
//!
//! What *can* go undrawn is declared instead.  The walk has three scissors
//! ([`BlockPrune`]): a depth in blocks, a budget of blocks, a budget of nodes,
//! and a fanout per side that keeps the first and last inputs and outputs of a
//! hub and stands an ellipsis for the rest.  A block the scissors did not admit
//! is not in the picture, and the outputs that lead to it are inked as a
//! frontier, exactly as `tree-pdf` inks a node the cut robbed of successors.
//!
//! # Where a root leads
//!
//! A node names a block two ways: the block that *produced* it (it is one of
//! the outputs) and the block that *consumes* it (it is one of the inputs).
//! `--root` reads as the producing block by default — the first output of a
//! block is then the block's name, as the tables key it — and falls back to
//! the consuming one for a source, which no block produced.  The choice is
//! said on stderr so that a caption never has to guess.  Several roots are
//! several seeds, side by side — unless one seed's outputs lead to another,
//! in which case the second is drawn under the first, as the chain it is, and
//! stderr says so.
//!
//! # Fetching a block
//!
//! The transpose gives an output's predecessors, which are the block's inputs;
//! the graph gives the first input's successors, which are the whole output
//! side.  Two probes.  The theorem's clauses that a gadget stands on are then
//! checked on the block just fetched — outputs contiguous, every input's
//! successors exactly O, every output's predecessors exactly I, inputs before
//! outputs — and a block that fails is a hard error naming it, because a
//! bipartite gadget of a non-block would be the drawing lying.  The second and
//! third of those are |I| + |O| probes, which `--no-check` may spare a drawing
//! of thousands of blocks; together they are what makes `dropped=0` a
//! certified statement rather than a promise: no arc into a drawn output
//! comes from anywhere but the drawn input side.  Random access only: memory
//! is proportional to the drawing, never to the graph.
//!
//! # Who is drawn where
//!
//! Roles are settled *after* the walk.  Every kept output of every drawn block
//! is a node on that block's output row.  An input of a drawn block is then
//! either one of those — drawn once, where its producer put it — or an
//! *outside input*: a node whose producing block is not on the page.  An
//! outside input is a source, or the output of a block the scissors left out
//! (drawn with a stub, "something made this") — told apart in [`Producer`],
//! and counted apart.
//!
//! Settling roles after the walk rather than at discovery is what makes
//! "outside" exact whatever the breadth-first order: a producer discovered
//! later in the same level still claims its outputs.  One more thing is
//! settled then: a node that is on the page cannot also be behind an ellipsis.
//! A fanout-hidden output that a drawn block consumes, or a fanout-hidden
//! input that a drawn block produced, is un-hidden — kept at its own place in
//! its row, the ellipsis splitting around it — so that a node is drawn once,
//! where its producer put it, and never stands twice on the page.
//!
//! # The layout
//!
//! Every input of a block sits on the row above the block's outputs.  That is
//! the whole rule; the boxes of the tidy-tree layout are arranged to keep it.
//! A block is one box, as broad as its output row and one row deep, hung
//! under the block that fed it; its drawn inputs are then on the parent's
//! output row, which is the row above.  Its outside inputs go in a box of
//! their own, *beside* the parent's box on the same row, so that they too are
//! one row above the outputs they feed and no arc of the gadget is longer
//! than a row.  A seed has no parent to sit under, so its outside inputs are a
//! row of its own box, which is two rows deep.  Rows are one unit apart
//! everywhere, as levels are everywhere else in this crate; inside its box a
//! row is centred, its nodes two units apart as siblings are.
//!
//! An arc that spans exactly one row can pass over nothing, since rows are
//! the only places nodes are.  The arcs that can are the quotient's non-tree
//! edges: an input that is an output of a drawn block *other* than the parent
//! is a *cross* arc, drawn to the node where it already is and counted, never
//! duplicated — and it may skip rows.  Whether any arc crosses a row within a
//! unit of a node it does not end at is checked after placement and reported
//! as `over_nodes=`, so that a caption can say the picture has none.  And the
//! one arc between two drawn nodes that no drawn block owns — from a frontier
//! output to an outside input, an arc of a block the scissors cut — is found
//! exactly by asking the transpose about every outside input, drawn dotted in
//! the cut's colour, and counted as *stray*.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use non_layered_tidy_trees::{Arena, NodeId};
use webgraph::prelude::RandomAccessGraph;

use crate::camera::Rect;
use crate::forest::{self, DIAMETER, SUBTREE_MARGIN};

/// Blocks the walk may draw when the caller does not say.
pub const DEFAULT_MAX_BLOCKS: usize = 20_000;

/// How close to a node an arc may cross the node's row before the arc is
/// counted as passing over it, in node units of breadth: half a slot.
pub const CLEARANCE: f64 = 1.0;

/// What `--fanout` kept of one side of a block: everything, or the first
/// `head` and the last `tail` entries plus any the page needed un-hidden,
/// with the rest standing behind an ellipsis.
#[derive(Clone, Debug, PartialEq)]
pub enum Kept {
    All,
    Ends {
        head: usize,
        tail: usize,
        /// Indices strictly between the head and the tail kept because the
        /// node is on the page anyway (see the module's fifth section), in
        /// increasing order.
        extra: Vec<usize>,
    },
}

/// One place on a row: a kept entry of the side, or the run of hidden ones
/// between two kept entries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Node(usize),
    Gap(usize),
}

impl Kept {
    /// What a cap of `fanout` keeps of `n` entries: both ends, half the
    /// allowance each — the same rule as [`forest::Prune::fanout`], and for
    /// the same reason: a block's outputs are a contiguous id range and what
    /// hangs off the last is not what hangs off the first.
    pub fn of(n: usize, fanout: Option<usize>) -> Kept {
        match fanout {
            Some(k) if n > k => {
                let head = k.div_ceil(2);
                Kept::Ends { head, tail: k - head, extra: Vec::new() }
            }
            _ => Kept::All,
        }
    }

    /// The indices actually drawn, in id order.
    pub fn indices(&self, n: usize) -> Vec<usize> {
        match self {
            Kept::All => (0..n).collect(),
            Kept::Ends { head, tail, extra } => {
                (0..*head).chain(extra.iter().copied()).chain(n - tail..n).collect()
            }
        }
    }

    pub fn contains(&self, n: usize, i: usize) -> bool {
        match self {
            Kept::All => i < n,
            Kept::Ends { head, tail, extra } => i < *head || i >= n - tail || extra.contains(&i),
        }
    }

    /// Un-hides entry `i`: kept from now on, at its own place.
    pub fn keep(&mut self, n: usize, i: usize) {
        if self.contains(n, i) {
            return;
        }
        if let Kept::Ends { extra, .. } = self {
            let at = extra.partition_point(|&e| e < i);
            extra.insert(at, i);
        }
    }

    /// The row as it is drawn: every kept entry at its place and one gap,
    /// saying how many, for every run of hidden entries between two kept ones
    /// or at either end.
    pub fn slots(&self, n: usize) -> Vec<Slot> {
        let mut out = Vec::new();
        let mut prev: Option<usize> = None;
        for i in self.indices(n) {
            let hidden = match prev {
                None => i,
                Some(p) => i - p - 1,
            };
            if hidden > 0 {
                out.push(Slot::Gap(hidden));
            }
            out.push(Slot::Node(i));
            prev = Some(i);
        }
        let after = match prev {
            None => n,
            Some(p) => n - 1 - p,
        };
        if after > 0 {
            out.push(Slot::Gap(after));
        }
        out
    }

    pub fn count(&self, n: usize) -> usize {
        match self {
            Kept::All => n,
            Kept::Ends { head, tail, extra } => head + tail + extra.len(),
        }
    }

    pub fn hidden(&self, n: usize) -> usize {
        n - self.count(n)
    }

    /// Whether anything is hidden: a side whose every hidden entry was
    /// un-hidden is whole again.
    pub fn is_cut(&self, n: usize) -> bool {
        self.hidden(n) > 0
    }
}

/// One complete bipartite block K(I, O) of the theorem, keyed by its first
/// output — the id the tables use.
#[derive(Clone, Debug)]
pub struct Block {
    /// `O[0]`: the block's identity.
    pub first_out: usize,
    /// I = pre(first_out), in transpose successor order, which is id order.
    pub inputs: Vec<usize>,
    /// `O = suc(I[0])`: contiguous, `[first_out, first_out + |O|)`.
    pub outputs: Vec<usize>,
    /// Depth in the drawn forest of blocks: a seed is 0, a block one arc
    /// under a seed is 1.
    pub level: usize,
    /// The quotient's spanning-tree parent: the block, and the output of it
    /// through which this one was discovered.  `None` for a seed.
    pub parent: Option<(u32, usize)>,
    pub kept_in: Kept,
    pub kept_out: Kept,
}

impl Block {
    /// How many arcs the block has: |I|·|O|.
    pub fn arcs(&self) -> u64 {
        self.inputs.len() as u64 * self.outputs.len() as u64
    }

    /// `K(2,2) at 564`: how a block is named in messages.
    pub fn name(&self) -> String {
        format!("K({},{}) at {}", self.inputs.len(), self.outputs.len(), self.first_out)
    }
}

/// Where a walk over blocks is told to stop — in blocks, since that is the
/// unit here.
///
/// - `depth`: block levels drawn below the seed, the seed being level 0.  A
///   block on the last level is drawn whole, and what its outputs feed is not.
/// - `max_blocks`, `max_nodes`: budgets, spent breadth first on the *nearest*
///   blocks, as `tree-pdf`'s budget buys the nearest nodes.
/// - `fanout_in`, `fanout_out`: how many inputs and outputs a block may show;
///   see [`Kept`].
pub struct BlockPrune {
    pub depth: Option<usize>,
    pub max_blocks: usize,
    pub max_nodes: usize,
    pub fanout_in: Option<usize>,
    pub fanout_out: Option<usize>,
}

impl Default for BlockPrune {
    fn default() -> Self {
        BlockPrune {
            depth: None,
            max_blocks: DEFAULT_MAX_BLOCKS,
            max_nodes: usize::MAX,
            fanout_in: None,
            fanout_out: None,
        }
    }
}

impl BlockPrune {
    /// What the scissors are set to, for stderr.
    pub fn summary(&self) -> String {
        let fanout = match (self.fanout_in, self.fanout_out) {
            (None, None) => "none".to_string(),
            (i, o) => format!(
                "in={} out={}",
                i.map_or("none".to_string(), |n| n.to_string()),
                o.map_or("none".to_string(), |n| n.to_string())
            ),
        };
        format!(
            "walk: depth {} (blocks), max-blocks {}, max-nodes {}, fanout {}",
            self.depth.map_or("none".to_string(), |d| d.to_string()),
            self.max_blocks,
            if self.max_nodes == usize::MAX { "none".to_string() } else { self.max_nodes.to_string() },
            fanout
        )
    }
}

/// Which block a `--root` names: the one that produced it, or the one that
/// consumes it.  See the module's third section.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seed {
    Producing,
    Consuming,
}

/// The numbers a caption quotes.  Every field is printed by [`Report::lines`],
/// one `key=value` per quantity under stable names, `dropped=0` included ---
/// printed on every run precisely so that the caption can quote it.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct Report {
    pub blocks_drawn: usize,
    pub deepest_level: usize,
    pub blocks_cut_by_fanout: usize,
    /// Discovered but not admitted, by the block or the node budget.
    pub blocks_refused_by_budget: usize,
    /// Distinct consuming blocks of frontier outputs that the depth stopped,
    /// or that a hidden output leads to: what the picture stops short of.
    pub blocks_beyond_depth: usize,
    /// Blocks whose inputs are partly drawn outputs and partly outside inputs
    /// (or inputs the fanout hid): the ones with an input box beside their
    /// parent.
    pub blocks_mixed: usize,
    /// Blocks re-verified as complete bipartite on fetch.
    pub blocks_checked: usize,
    pub nodes_drawn: usize,
    pub outputs_drawn: usize,
    pub outside_inputs: usize,
    /// Of the outside inputs, the true sources.
    pub outside_sources: usize,
    /// Of the outside inputs, the outputs of blocks not on the page.
    pub outside_from_undrawn: usize,
    pub inputs_hidden: usize,
    pub outputs_hidden: usize,
    /// Hidden outputs with an out-degree: each leads to a block the ellipsis
    /// stands in front of, counted in `blocks_beyond_depth` unless that block
    /// is on the page.
    pub hidden_outputs_with_consumer: usize,
    pub arcs_drawn: u64,
    /// Σ |I||O| over the drawn blocks.
    pub arcs_in_drawn_blocks: u64,
    pub arcs_hidden_by_fanout: u64,
    pub arcs_cross: u64,
    pub arcs_stray: u64,
    /// Always 0; the field exists to be printed.
    pub arcs_dropped: u64,
    /// Drawn arcs that cross a row within [`CLEARANCE`] of a node or an
    /// ellipsis they do not end at: what a reader could misread as a path
    /// through that node.  0 unless a cross or stray arc skips a row.
    pub arcs_over_nodes: u64,
    /// Drawn nodes with an out-degree and no drawn out-arc.
    pub frontier_outputs: usize,
    pub sinks: usize,
}

impl Report {
    /// The four report lines, in the order and under the names a script can
    /// rely on.
    pub fn lines(&self) -> String {
        format!(
            "blocks: drawn={} deepest_level={} cut_by_fanout={} refused_by_budget={} beyond_depth={} mixed_blocks={} checked={} violations=0\n\
             nodes: drawn={} outputs={} outside_inputs={} (sources={}, from_undrawn_blocks={}) hidden_inputs={} hidden_outputs={} hidden_outputs_with_consumer={}\n\
             arcs: drawn={} in_drawn_blocks={} hidden_by_fanout={} cross={} stray={} dropped={} over_nodes={}\n\
             frontier: outputs_with_undrawn_consumer={} sinks={}",
            self.blocks_drawn,
            self.deepest_level,
            self.blocks_cut_by_fanout,
            self.blocks_refused_by_budget,
            self.blocks_beyond_depth,
            self.blocks_mixed,
            self.blocks_checked,
            self.nodes_drawn,
            self.outputs_drawn,
            self.outside_inputs,
            self.outside_sources,
            self.outside_from_undrawn,
            self.inputs_hidden,
            self.outputs_hidden,
            self.hidden_outputs_with_consumer,
            self.arcs_drawn,
            self.arcs_in_drawn_blocks,
            self.arcs_hidden_by_fanout,
            self.arcs_cross,
            self.arcs_stray,
            self.arcs_dropped,
            self.arcs_over_nodes,
            self.frontier_outputs,
            self.sinks,
        )
    }
}

fn successors_of<G: RandomAccessGraph>(graph: &G, u: usize) -> Vec<usize> {
    graph.successors(u).into_iter().collect()
}

/// The block one of whose outputs `key` is: inputs from the transpose, outputs
/// from the first input, and the theorem's clauses checked on the result.
///
/// `check` is clauses (ii) and (iii) --- every output's predecessors being
/// exactly I and every input's successors exactly O: |O| + |I| - 1 probes,
/// which `--no-check` may spare a drawing of thousands of blocks; the
/// contiguity of O and the order of I before O cost nothing and are always
/// checked.  Level, parent and cut are the walk's to fill in.
pub fn fetch_block<G: RandomAccessGraph, T: RandomAccessGraph>(
    pg: &G,
    pgt: &T,
    key: usize,
    check: bool,
) -> Result<Block, String> {
    let inputs = successors_of(pgt, key);
    if inputs.is_empty() {
        return Err(format!("node {key} has no predecessor: it is not the output of any block"));
    }
    let outputs = successors_of(pg, inputs[0]);
    let first_out = outputs[0];
    if !(first_out <= key && key < first_out + outputs.len()) {
        return Err(format!(
            "node {key} is not among the successors [{}..{}] of its own first predecessor {}: not a block",
            first_out,
            first_out + outputs.len() - 1,
            inputs[0]
        ));
    }
    for (k, &o) in outputs.iter().enumerate() {
        if o != first_out + k {
            return Err(format!(
                "the outputs of the block at {first_out} are not contiguous: {o} where {} was expected",
                first_out + k
            ));
        }
    }
    if *inputs.last().unwrap() >= first_out {
        return Err(format!(
            "the block at {first_out} has an input {} at or after its first output",
            inputs.last().unwrap()
        ));
    }
    if check {
        for &u in &inputs[1..] {
            let suc = successors_of(pg, u);
            if suc != outputs {
                return Err(format!(
                    "the block at {first_out} is not complete bipartite: input {u} has {} successors starting at {:?}, not the block's {} outputs",
                    suc.len(),
                    suc.first(),
                    outputs.len()
                ));
            }
        }
        for &o in &outputs[1..] {
            let pre = successors_of(pgt, o);
            if pre != inputs {
                let odd = pre.iter().find(|u| !inputs.contains(u)).copied();
                return Err(match odd {
                    Some(u) => format!(
                        "the block at {first_out} is not complete bipartite: output {o} has a predecessor {u} that is not among the block's {} inputs",
                        inputs.len()
                    ),
                    None => format!(
                        "the block at {first_out} is not complete bipartite: output {o} has {} predecessors, not the block's {} inputs",
                        pre.len(),
                        inputs.len()
                    ),
                });
            }
        }
    }
    Ok(Block {
        first_out,
        inputs,
        outputs,
        level: 0,
        parent: None,
        kept_in: Kept::All,
        kept_out: Kept::All,
    })
}

/// The block a `--root` names, and the sentence saying which reading was
/// taken, for stderr.
pub fn seed_of<G: RandomAccessGraph, T: RandomAccessGraph>(
    pg: &G,
    pgt: &T,
    root: usize,
    mode: Seed,
    check: bool,
) -> Result<(Block, String), String> {
    let n = pg.num_nodes();
    if root >= n {
        return Err(format!("node {root} is not in a graph of {n} nodes"));
    }
    let indeg = pgt.outdegree(root);
    let outdeg = pg.outdegree(root);
    if mode == Seed::Producing && indeg > 0 {
        let b = fetch_block(pg, pgt, root, check)?;
        let msg = format!("seed: root {root} is an output of {}", b.name());
        return Ok((b, msg));
    }
    if outdeg > 0 {
        let first = pg.successors(root).into_iter().next().unwrap();
        let b = fetch_block(pg, pgt, first, check)?;
        let msg = if indeg == 0 {
            format!("seed: root {root} is a source (in-degree 0); drawing the block it feeds, {}", b.name())
        } else {
            format!("seed: root {root} is an input of {}", b.name())
        };
        return Ok((b, msg));
    }
    if indeg > 0 {
        let b = fetch_block(pg, pgt, root, check)?;
        let msg = format!(
            "seed: root {root} is a sink (out-degree 0); drawing the block that produced it, {}",
            b.name()
        );
        return Ok((b, msg));
    }
    Err(format!("node {root} is isolated: no block contains it"))
}

/// What the walk produced: the blocks in discovery order, the seed sentences,
/// and the part of the report the walk alone can fill.
pub struct Walk {
    pub blocks: Vec<Block>,
    pub seeds: Vec<String>,
    pub report: Report,
    pub checked: bool,
}

/// The breadth-first walk over blocks from the blocks the roots name, under
/// `prune`.
///
/// A block is expanded by following each kept output to the block that
/// consumes it — one probe to know it has one, one to know its name — and
/// that block is admitted if the budgets allow, refused and remembered if not.
/// A block on the last level is drawn and not expanded; the blocks its
/// outputs lead to are counted as beyond the depth, so that the caption can
/// say how many the picture stops short of.  Hidden outputs are not followed
/// — their consumer would have an undrawn input — but each is asked whether it
/// leads anywhere, so that the ellipsis never hides an unknown number of
/// blocks.
///
/// An output that leads to another *seed* adopts it: the seed is drawn under
/// the block that fed it rather than beside it, its level counted from there.
/// And once the walk is over, whatever the fanout hid that the page shows
/// anyway is un-hidden (the module's fifth section).
///
/// Budgets are checked from degrees *before* a block is fetched: a refused
/// block costs two probes, not its certification.
pub fn walk<G: RandomAccessGraph, T: RandomAccessGraph>(
    pg: &G,
    pgt: &T,
    roots: &[usize],
    prune: &BlockPrune,
    mode: Seed,
    check: bool,
) -> Result<Walk, String> {
    if roots.is_empty() {
        return Err("no root to draw from".to_string());
    }
    if prune.max_blocks == 0 || prune.max_nodes == 0 {
        return Err("a budget of 0 draws nothing".to_string());
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut by_key: HashMap<usize, u32> = HashMap::new();
    let mut queue: VecDeque<u32> = VecDeque::new();
    let mut seeds: Vec<String> = Vec::new();
    let mut report = Report::default();
    let mut nodes_used = 0usize;
    // Consuming blocks the drawing stops in front of, by name: those a
    // frontier output or a hidden output leads to, and those refused.
    let mut beyond: HashSet<usize> = HashSet::new();
    let mut refused: HashSet<usize> = HashSet::new();
    // Hidden outputs that lead somewhere, by id: counted at the end, once
    // un-hiding has had its say.
    let mut hidden_with_consumer: HashSet<usize> = HashSet::new();

    // Whether the budgets seat a block of these sides, and what the fanout
    // keeps of it if so.
    let budget = |blocks_len: usize, nodes_used: usize, n_in: usize, n_out: usize| -> Option<(Kept, Kept, usize)> {
        if blocks_len >= prune.max_blocks {
            return None;
        }
        let kept_in = Kept::of(n_in, prune.fanout_in);
        let kept_out = Kept::of(n_out, prune.fanout_out);
        // An upper bound: inputs already drawn as outputs are not new nodes.
        let cost = kept_in.count(n_in) + kept_out.count(n_out);
        if nodes_used + cost > prune.max_nodes {
            return None;
        }
        Some((kept_in, kept_out, cost))
    };

    for &r in roots {
        let (mut b, msg) = seed_of(pg, pgt, r, mode, check)?;
        if by_key.contains_key(&b.first_out) {
            seeds.push(format!("{msg} (already drawn)"));
            continue;
        }
        let Some((kept_in, kept_out, cost)) =
            budget(blocks.len(), nodes_used, b.inputs.len(), b.outputs.len())
        else {
            return Err(format!(
                "the budget was spent before root {r} was seated; raise --max-blocks or --max-nodes"
            ));
        };
        b.kept_in = kept_in;
        b.kept_out = kept_out;
        nodes_used += cost;
        seeds.push(msg);
        let idx = blocks.len() as u32;
        by_key.insert(b.first_out, idx);
        blocks.push(b);
        queue.push_back(idx);
    }

    while let Some(bi) = queue.pop_front() {
        let level = blocks[bi as usize].level;
        let expand = !prune.depth.is_some_and(|d| level >= d);
        let n_out = blocks[bi as usize].outputs.len();
        let kept: Vec<usize> = blocks[bi as usize]
            .kept_out
            .indices(n_out)
            .into_iter()
            .map(|oi| blocks[bi as usize].outputs[oi])
            .collect();
        let hidden: Vec<usize> = {
            let b = &blocks[bi as usize];
            (0..n_out).filter(|&oi| !b.kept_out.contains(n_out, oi)).map(|oi| b.outputs[oi]).collect()
        };

        // A seed this block's output feeds is drawn under it, not beside it.
        let adopt = |blocks: &mut Vec<Block>, seeds: &mut Vec<String>, by_key: &HashMap<usize, u32>, key: usize, o: usize| {
            let ci = by_key[&key] as usize;
            if ci != bi as usize && blocks[ci].parent.is_none() {
                blocks[ci].parent = Some((bi, o));
                seeds.push(format!(
                    "seed {} is reached from {} through output {o}: drawn under it",
                    blocks[ci].name(),
                    blocks[bi as usize].name()
                ));
            }
        };

        for &o in &hidden {
            if pg.outdegree(o) > 0 {
                hidden_with_consumer.insert(o);
                let key = pg.successors(o).into_iter().next().unwrap();
                if by_key.contains_key(&key) {
                    adopt(&mut blocks, &mut seeds, &by_key, key, o);
                } else {
                    beyond.insert(key);
                }
            }
        }

        for &o in &kept {
            let outdeg = pg.outdegree(o);
            if outdeg == 0 {
                continue;
            }
            let key = pg.successors(o).into_iter().next().unwrap();
            if by_key.contains_key(&key) {
                // Already discovered: the quotient edge is a cross edge
                // unless that block's parent is this one, and the arcs o -> O
                // are drawn by that block either way.
                adopt(&mut blocks, &mut seeds, &by_key, key, o);
                continue;
            }
            if !expand {
                beyond.insert(key);
                continue;
            }
            if refused.contains(&key) {
                continue;
            }
            // Clause (iii) makes o's successors the whole of O, so |O| is o's
            // out-degree and |I| the key's in-degree: enough for the budget.
            let Some((kept_in, kept_out, cost)) =
                budget(blocks.len(), nodes_used, pgt.outdegree(key), outdeg)
            else {
                refused.insert(key);
                continue;
            };
            let mut c = fetch_block(pg, pgt, key, check)?;
            c.level = level + 1;
            c.parent = Some((bi, o));
            c.kept_in = kept_in;
            c.kept_out = kept_out;
            nodes_used += cost;
            let idx = blocks.len() as u32;
            by_key.insert(c.first_out, idx);
            blocks.push(c);
            queue.push_back(idx);
        }
    }

    unhide(&mut blocks);
    relevel(&mut blocks);

    report.blocks_drawn = blocks.len();
    report.deepest_level = blocks.iter().map(|b| b.level).max().unwrap_or(0);
    report.blocks_checked = if check { blocks.len() } else { 0 };
    report.blocks_refused_by_budget = refused.len();
    report.blocks_beyond_depth =
        beyond.iter().filter(|k| !by_key.contains_key(k) && !refused.contains(k)).count();
    for b in &blocks {
        if b.kept_in.is_cut(b.inputs.len()) || b.kept_out.is_cut(b.outputs.len()) {
            report.blocks_cut_by_fanout += 1;
        }
        report.inputs_hidden += b.kept_in.hidden(b.inputs.len());
        report.outputs_hidden += b.kept_out.hidden(b.outputs.len());
        report.arcs_in_drawn_blocks += b.arcs();
        let n_out = b.outputs.len();
        for (oi, o) in b.outputs.iter().enumerate() {
            if b.kept_out.contains(n_out, oi) {
                hidden_with_consumer.remove(o);
            }
        }
    }
    report.hidden_outputs_with_consumer = hidden_with_consumer.len();

    Ok(Walk { blocks, seeds, report, checked: check })
}

/// Un-hides what the page shows anyway: a hidden output that a drawn block
/// consumes through a kept input, and a hidden input that a drawn block
/// produced as a kept output.  One pass settles it: an entry un-hidden by one
/// rule is by construction not a case for the other.
fn unhide(blocks: &mut [Block]) {
    // Every input of every drawn block, kept or hidden, to its block and its
    // index; every drawn block by its output range.
    let mut input_at: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut ranges: BTreeMap<usize, usize> = BTreeMap::new();
    for (bi, b) in blocks.iter().enumerate() {
        for (ii, &u) in b.inputs.iter().enumerate() {
            input_at.insert(u, (bi, ii));
        }
        ranges.insert(b.first_out, bi);
    }
    let producer_of = |u: usize| -> Option<usize> {
        ranges
            .range(..=u)
            .next_back()
            .map(|(_, &qi)| qi)
            .filter(|&qi| u < blocks[qi].first_out + blocks[qi].outputs.len())
    };

    let mut keep_out: Vec<(usize, usize)> = Vec::new();
    for b in blocks.iter() {
        for ii in b.kept_in.indices(b.inputs.len()) {
            let u = b.inputs[ii];
            if let Some(qi) = producer_of(u) {
                let q = &blocks[qi];
                let oi = u - q.first_out;
                if !q.kept_out.contains(q.outputs.len(), oi) {
                    keep_out.push((qi, oi));
                }
            }
        }
    }
    for (qi, oi) in keep_out {
        let n = blocks[qi].outputs.len();
        blocks[qi].kept_out.keep(n, oi);
    }

    let mut keep_in: Vec<(usize, usize)> = Vec::new();
    for q in blocks.iter() {
        for oi in q.kept_out.indices(q.outputs.len()) {
            if let Some(&(bi, ii)) = input_at.get(&q.outputs[oi]) {
                if !blocks[bi].kept_in.contains(blocks[bi].inputs.len(), ii) {
                    keep_in.push((bi, ii));
                }
            }
        }
    }
    for (bi, ii) in keep_in {
        let n = blocks[bi].inputs.len();
        blocks[bi].kept_in.keep(n, ii);
    }
}

/// Levels from the parents, once adoption has settled who hangs under whom.
fn relevel(blocks: &mut [Block]) {
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (bi, b) in blocks.iter().enumerate() {
        match b.parent {
            Some((p, _)) => children[p as usize].push(bi),
            None => queue.push_back(bi),
        }
    }
    for bi in &queue {
        blocks[*bi].level = 0;
    }
    while let Some(bi) = queue.pop_front() {
        let level = blocks[bi].level + 1;
        for &c in &children[bi] {
            blocks[c].level = level;
            queue.push_back(c);
        }
    }
}

/// Who made an outside input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Producer {
    /// Nothing: a true source.
    Source,
    /// A block the scissors left out: drawn with a stub.
    Undrawn,
}

/// Where a drawn node sits and what it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    /// Output of drawn block `block`, on its output row.
    Output { block: u32 },
    /// Input of drawn block `block` that no drawn block put on the page, on
    /// that block's input row.
    Outside { block: u32, producer: Producer },
}

/// One drawn node: the identity of a graph node on the page.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub graph: usize,
    pub role: Role,
    /// Centre, in node units; the depth axis is `y` when vertical.
    pub x: f64,
    pub y: f64,
    /// Its place on its row, ellipsis slot included.
    pub slot: usize,
    pub outdeg: usize,
    /// A kept input of a drawn block: it has drawn out-arcs.
    pub consumed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArcKind {
    Tree,
    Cross,
    Stray,
}

/// One drawn arc, by indices into the nodes.
#[derive(Clone, Copy, Debug)]
pub struct Arc {
    pub from: u32,
    pub to: u32,
    pub kind: ArcKind,
}

/// Which row of a block a mark sits on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Row {
    Input,
    Output,
}

/// A mark standing for a fanout-hidden run: three dots in the cut's colour at
/// a slot of its own, so that the row's spacing stays uniform.
#[derive(Clone, Copy, Debug)]
pub struct Ellipsis {
    pub x: f64,
    pub y: f64,
    pub hidden: usize,
    pub block: u32,
    pub row: Row,
    pub slot: usize,
}

/// Everything the page needs, already laid out: the block analogue of
/// `tree-pdf`'s `Scene`.
pub struct BlockScene {
    pub blocks: Vec<Block>,
    pub nodes: Vec<Placed>,
    /// Graph id to node index: one entry per drawn node, which is what makes
    /// a node's identity on the page.
    pub index: HashMap<usize, u32>,
    pub arcs: Vec<Arc>,
    pub ellipses: Vec<Ellipsis>,
    /// In node units, circles included.
    pub bounds: Rect,
    pub report: Report,
    pub vertical: bool,
}

/// Breadth-axis centre of slot `k` of a row of `n` slots centred in a box of
/// breadth `w` starting at `s0`.
fn row_centre(s0: f64, w: f64, n: usize, k: usize) -> f64 {
    s0 + (w - (2.0 * n as f64 - 1.0)) / 2.0 + 2.0 * k as f64 + 0.5
}

/// One place on a row as it is drawn: a node by index, or an ellipsis.
#[derive(Clone, Copy, Debug)]
enum RowSlot {
    Node(u32),
    Gap(usize),
}

/// A box of the tidy tree: a block, or the outside inputs of one.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Block(usize),
    Outside(usize),
}

/// Settles the roles, lays the blocks out as boxes of the tidy tree, places
/// every node and ellipsis, lists the arcs by kind, and checks what they pass
/// over.
pub fn lay_out_blocks<G: RandomAccessGraph, T: RandomAccessGraph>(
    pg: &G,
    pgt: &T,
    walk: Walk,
    vertical: bool,
) -> Result<BlockScene, String> {
    let Walk { blocks, mut report, .. } = walk;
    let mut nodes: Vec<Placed> = Vec::new();
    let mut index: HashMap<usize, u32> = HashMap::new();

    // 1. Outputs first, so that the role of an input can be looked up exactly.
    for (bi, b) in blocks.iter().enumerate() {
        for oi in b.kept_out.indices(b.outputs.len()) {
            let o = b.outputs[oi];
            index.insert(o, nodes.len() as u32);
            nodes.push(Placed {
                graph: o,
                role: Role::Output { block: bi as u32 },
                x: 0.0,
                y: 0.0,
                slot: 0,
                outdeg: pg.outdegree(o),
                consumed: false,
            });
        }
    }

    // 2. Inputs: the existing node, or an outside input on the block's input
    // row.  The row itself is settled here too: a slot per outside input and
    // per hidden run, the drawn inputs being on rows of their own.
    let mut in_rows: Vec<Vec<RowSlot>> = Vec::with_capacity(blocks.len());
    let mut out_rows: Vec<Vec<RowSlot>> = Vec::with_capacity(blocks.len());
    for (bi, b) in blocks.iter().enumerate() {
        let (mut drawn_in, mut row) = (0usize, Vec::new());
        for slot in b.kept_in.slots(b.inputs.len()) {
            match slot {
                Slot::Gap(h) => match row.last_mut() {
                    Some(RowSlot::Gap(g)) => *g += h,
                    _ => row.push(RowSlot::Gap(h)),
                },
                Slot::Node(ii) => {
                    let u = b.inputs[ii];
                    if let Some(&i) = index.get(&u) {
                        nodes[i as usize].consumed = true;
                        drawn_in += 1;
                        continue;
                    }
                    let producer =
                        if pgt.outdegree(u) == 0 { Producer::Source } else { Producer::Undrawn };
                    let i = nodes.len() as u32;
                    index.insert(u, i);
                    nodes.push(Placed {
                        graph: u,
                        role: Role::Outside { block: bi as u32, producer },
                        x: 0.0,
                        y: 0.0,
                        slot: 0,
                        outdeg: pg.outdegree(u),
                        consumed: true,
                    });
                    row.push(RowSlot::Node(i));
                }
            }
        }
        if drawn_in > 0 && !row.is_empty() {
            report.blocks_mixed += 1;
        }
        in_rows.push(row);
        out_rows.push(
            b.kept_out
                .slots(b.outputs.len())
                .into_iter()
                .map(|s| match s {
                    Slot::Node(oi) => RowSlot::Node(index[&b.outputs[oi]]),
                    Slot::Gap(h) => RowSlot::Gap(h),
                })
                .collect(),
        );
    }

    // 3. Boxes of the tidy tree: a block is a box as broad as its output row
    // and one row deep, under its parent; a seed's input row is a second row
    // of its own box; any other block's input row is a box beside the parent.
    let seed = |bi: usize| blocks[bi].parent.is_none();
    let breadth_of = |n: usize| (2 * n.max(1) - 1) as f64 * DIAMETER;
    let box_depth: Vec<f64> = (0..blocks.len())
        .map(|bi| if seed(bi) && !in_rows[bi].is_empty() { 2.0 * DIAMETER } else { DIAMETER })
        .collect();

    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    let mut top: Vec<usize> = Vec::new();
    for (bi, b) in blocks.iter().enumerate() {
        match b.parent {
            Some((p, _)) => kids[p as usize].push(bi),
            None => top.push(bi),
        }
    }
    // The input box of a child goes on the side of the parent the child is
    // on, so that its arcs stay short: first half of the children before the
    // parent, the rest after.
    let mut before: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    let mut after: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    for (bi, b) in blocks.iter().enumerate() {
        let Some((p, _)) = b.parent else { continue };
        if in_rows[bi].is_empty() {
            continue;
        }
        let p = p as usize;
        let pos = kids[p].iter().position(|&c| c == bi).unwrap();
        if 2 * pos < kids[p].len() - 1 {
            before[p].push(bi);
        } else {
            after[p].push(bi);
        }
    }
    let entries = |list: &[usize]| -> Vec<Entry> {
        let mut out = Vec::new();
        for &p in list {
            out.extend(before[p].iter().map(|&c| Entry::Outside(c)));
            out.push(Entry::Block(p));
            out.extend(after[p].iter().map(|&c| Entry::Outside(c)));
        }
        out
    };

    let mut arena = Arena::new();
    let (mut box_ids, mut in_box_ids): (Vec<NodeId>, Vec<Option<NodeId>>) =
        (Vec::with_capacity(blocks.len()), vec![None; blocks.len()]);
    let sized = |breadth: f64, depth: f64| if vertical { (breadth, depth) } else { (depth, breadth) };
    for bi in 0..blocks.len() {
        let mut breadth = breadth_of(out_rows[bi].len());
        if seed(bi) {
            breadth = breadth.max(breadth_of(in_rows[bi].len()));
        }
        let (w, h) = sized(breadth, box_depth[bi]);
        box_ids.push(arena.add_node(bi + 1, w, h, SUBTREE_MARGIN, false));
    }
    for (bi, b) in blocks.iter().enumerate() {
        let Some((p, _)) = b.parent else { continue };
        if in_rows[bi].is_empty() {
            continue;
        }
        let (w, h) = sized(breadth_of(in_rows[bi].len()), box_depth[p as usize]);
        in_box_ids[bi] = Some(arena.add_node(blocks.len() + bi + 1, w, h, SUBTREE_MARGIN, false));
    }
    let id_of = |e: Entry| match e {
        Entry::Block(bi) => box_ids[bi],
        Entry::Outside(bi) => in_box_ids[bi].unwrap(),
    };
    for p in 0..blocks.len() {
        for e in entries(&kids[p]) {
            arena.push_child(box_ids[p], id_of(e));
        }
    }
    let top: Vec<NodeId> = entries(&top).into_iter().map(id_of).collect();
    let root = if top.len() > 1 {
        // Zero by zero, exactly as in `forest`: the seeds land where they
        // would have landed on their own.
        let r = arena.add_node(0, 0.0, 0.0, 0.0, true);
        arena.set_children(r, &top);
        r
    } else {
        top[0]
    };
    let arena = forest::lay_out_oriented(arena, root, vertical);

    // 4. Rows inside the boxes.
    let mut ellipses: Vec<Ellipsis> = Vec::new();
    // Breadth and depth to the page's axes.
    let place = |s: f64, t: f64| if vertical { (s, t) } else { (t, s) };
    let frame = |id: NodeId| {
        let node = &arena[id];
        if vertical {
            (node.x, node.y, node.w, node.h)
        } else {
            (node.y, node.x, node.h, node.w)
        }
    };
    let mut lay_row = |row: &[RowSlot], s0: f64, breadth: f64, t: f64, bi: usize, which: Row, nodes: &mut Vec<Placed>| {
        for (k, slot) in row.iter().enumerate() {
            let (x, y) = place(row_centre(s0, breadth, row.len(), k), t);
            match *slot {
                RowSlot::Node(i) => {
                    let p = &mut nodes[i as usize];
                    (p.x, p.y) = (x, y);
                    p.slot = k;
                }
                RowSlot::Gap(hidden) => {
                    ellipses.push(Ellipsis { x, y, hidden, block: bi as u32, row: which, slot: k })
                }
            }
        }
    };
    for bi in 0..blocks.len() {
        let (s0, t0, breadth, depth) = frame(box_ids[bi]);
        // The output row, at the far edge of the box.
        lay_row(&out_rows[bi], s0, breadth, t0 + depth - 0.5 * DIAMETER, bi, Row::Output, &mut nodes);
        if in_rows[bi].is_empty() {
            continue;
        }
        // The input row: the near edge of a seed's box, or the far edge of
        // the box beside the parent.
        match in_box_ids[bi] {
            None => lay_row(&in_rows[bi], s0, breadth, t0 + 0.5 * DIAMETER, bi, Row::Input, &mut nodes),
            Some(id) => {
                let (s0, t0, breadth, depth) = frame(id);
                lay_row(&in_rows[bi], s0, breadth, t0 + depth - 0.5 * DIAMETER, bi, Row::Input, &mut nodes);
            }
        }
    }
    drop(arena);

    // 5. The arcs of every drawn block, by kind.
    let mut arcs: Vec<Arc> = Vec::new();
    for (bi, b) in blocks.iter().enumerate() {
        let parent = b.parent.map(|(p, _)| p);
        for ii in b.kept_in.indices(b.inputs.len()) {
            let from = index[&b.inputs[ii]];
            let kind = match nodes[from as usize].role {
                Role::Output { block } if Some(block) == parent => ArcKind::Tree,
                Role::Outside { block, .. } if block as usize == bi => ArcKind::Tree,
                _ => ArcKind::Cross,
            };
            for oi in b.kept_out.indices(b.outputs.len()) {
                let to = index[&b.outputs[oi]];
                arcs.push(Arc { from, to, kind });
            }
        }
    }

    // 6. Stray arcs: into an outside input from a drawn node, which can only
    // be an arc of the undrawn block that produced it.
    for i in 0..nodes.len() {
        let Role::Outside { producer: Producer::Undrawn, .. } = nodes[i].role else {
            continue;
        };
        for u in pgt.successors(nodes[i].graph) {
            if let Some(&from) = index.get(&u) {
                arcs.push(Arc { from, to: i as u32, kind: ArcKind::Stray });
            }
        }
    }

    let mut bounds = Rect::nothing();
    for p in &nodes {
        bounds.add(p.x, p.y);
    }
    for e in &ellipses {
        bounds.add(e.x, e.y);
    }
    let bounds = bounds.grown(DIAMETER / 2.0);

    report.nodes_drawn = nodes.len();
    for p in &nodes {
        match p.role {
            Role::Output { .. } => report.outputs_drawn += 1,
            Role::Outside { producer, .. } => {
                report.outside_inputs += 1;
                match producer {
                    Producer::Source => report.outside_sources += 1,
                    Producer::Undrawn => report.outside_from_undrawn += 1,
                }
            }
        }
        if p.outdeg == 0 {
            report.sinks += 1;
        } else if !p.consumed {
            report.frontier_outputs += 1;
        }
    }
    for a in &arcs {
        match a.kind {
            ArcKind::Tree => report.arcs_drawn += 1,
            ArcKind::Cross => {
                report.arcs_drawn += 1;
                report.arcs_cross += 1;
            }
            ArcKind::Stray => report.arcs_stray += 1,
        }
    }
    report.arcs_hidden_by_fanout = report.arcs_in_drawn_blocks - report.arcs_drawn;
    report.arcs_dropped = 0;
    report.arcs_over_nodes = arcs_over_nodes(&nodes, &ellipses, &arcs, vertical);

    Ok(BlockScene { blocks, nodes, index, arcs, ellipses, bounds, report, vertical })
}

/// How many of `arcs` cross a row within [`CLEARANCE`] of a node or an
/// ellipsis that is not one of their ends.
///
/// Rows are the only places anything is drawn, so an arc can only pass over
/// something where it meets a row: at the breadth where it crosses, or, for
/// an arc along a row, anywhere strictly between its ends.  Meeting a row is
/// found by the row's depth, and the neighbours by binary search, so a mesh
/// of a million arcs that each span one row costs a million lookups.
fn arcs_over_nodes(nodes: &[Placed], ellipses: &[Ellipsis], arcs: &[Arc], vertical: bool) -> u64 {
    let axes = |x: f64, y: f64| if vertical { (x, y) } else { (y, x) };
    // Depth to a key: rows sit at exact multiples of a half unit, and 1/1024
    // is finer than any rounding the layout can leave.
    let key = |t: f64| (t * 1024.0).round() as i64;
    let mut rows: BTreeMap<i64, Vec<(f64, u32)>> = BTreeMap::new();
    for (i, p) in nodes.iter().enumerate() {
        let (s, t) = axes(p.x, p.y);
        rows.entry(key(t)).or_default().push((s, i as u32));
    }
    for e in ellipses {
        let (s, t) = axes(e.x, e.y);
        rows.entry(key(t)).or_default().push((s, u32::MAX));
    }
    for row in rows.values_mut() {
        row.sort_by(|a, b| a.0.total_cmp(&b.0));
    }

    let mut over = 0;
    for a in arcs {
        let (s0, t0) = axes(nodes[a.from as usize].x, nodes[a.from as usize].y);
        let (s1, t1) = axes(nodes[a.to as usize].x, nodes[a.to as usize].y);
        let (t_lo, t_hi) = (t0.min(t1), t0.max(t1));
        let hit = rows.range(key(t_lo)..=key(t_hi)).any(|(k, row)| {
            let t = *k as f64 / 1024.0;
            let (lo, hi) = if t0 == t1 {
                (s0.min(s1), s0.max(s1))
            } else {
                let s = s0 + (s1 - s0) * (t - t0) / (t1 - t0);
                (s - CLEARANCE, s + CLEARANCE)
            };
            let start = row.partition_point(|e| e.0 <= lo);
            row[start..]
                .iter()
                .take_while(|e| e.0 < hi)
                .any(|e| e.1 != a.from && e.1 != a.to)
        });
        if hit {
            over += 1;
        }
    }
    over
}

/// The transpose of a test graph, built by reversing the arc list: what a
/// walk over blocks needs beside the graph, and what the tests build by hand
/// for the reason [`forest::graph_of`] exists.
#[cfg(test)]
pub fn transpose_of(n: usize, arcs: &[(usize, usize)]) -> webgraph::prelude::VecGraph {
    let reversed: Vec<(usize, usize)> = arcs.iter().map(|&(u, v)| (v, u)).collect();
    forest::graph_of(n, &reversed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forest::graph_of;

    fn scene_of(
        n: usize,
        arcs: &[(usize, usize)],
        roots: &[usize],
        prune: &BlockPrune,
        vertical: bool,
    ) -> BlockScene {
        let pg = graph_of(n, arcs);
        let pgt = transpose_of(n, arcs);
        let w = walk(&pg, &pgt, roots, prune, Seed::Producing, true).unwrap();
        lay_out_blocks(&pg, &pgt, w, vertical).unwrap()
    }

    /// Where a graph node was placed, in node units.
    fn at(scene: &BlockScene, g: usize) -> (f64, f64) {
        let p = scene.nodes[scene.index[&g] as usize];
        (p.x, p.y)
    }

    const CHAIN: [(usize, usize); 8] = [(0, 1), (0, 2), (1, 3), (1, 4), (2, 3), (2, 4), (3, 5), (4, 5)];

    /// U1: a root names the block that produced it, a source the block it
    /// feeds, and an isolated node no block at all.
    ///
    /// The inputs come before the outputs in node order, as the theorem's
    /// clause (iv) has them and as [`fetch_block`] insists: a fixture with an
    /// input numbered after an output is refused, not read.
    #[test]
    fn a_root_names_a_block() {
        // K({0,1},{2,3}), and 4 on its own.
        let arcs = [(0, 2), (0, 3), (1, 2), (1, 3)];
        let pg = graph_of(5, &arcs);
        let pgt = transpose_of(5, &arcs);

        let (b, msg) = seed_of(&pg, &pgt, 2, Seed::Producing, true).unwrap();
        assert_eq!((b.first_out, b.inputs.clone(), b.outputs.clone()), (2, vec![0, 1], vec![2, 3]));
        assert_eq!(msg, "seed: root 2 is an output of K(2,2) at 2");
        assert_eq!(seed_of(&pg, &pgt, 3, Seed::Producing, true).unwrap().0.first_out, 2);

        let (b, msg) = seed_of(&pg, &pgt, 0, Seed::Producing, true).unwrap();
        assert_eq!(b.first_out, 2, "a source falls back to the block it feeds");
        assert!(msg.contains("is a source"), "{msg}");

        let (b, _) = seed_of(&pg, &pgt, 1, Seed::Consuming, true).unwrap();
        assert_eq!(b.first_out, 2);

        assert!(seed_of(&pg, &pgt, 4, Seed::Producing, true).is_err(), "isolated");
        assert!(seed_of(&pg, &pgt, 9, Seed::Producing, true).is_err(), "not in the graph");

        // An input after an output: clause (iv), refused whatever `check`.
        let arcs = [(0, 1), (0, 2), (3, 1), (3, 2)];
        let pg = graph_of(4, &arcs);
        let pgt = transpose_of(4, &arcs);
        let err = fetch_block(&pg, &pgt, 1, false).unwrap_err();
        assert!(err.contains("input 3"), "{err}");
    }

    /// U2: a block that is not complete bipartite is refused, naming what
    /// breaks it --- an input with the wrong successors, or an output with a
    /// predecessor from outside the block; `check: false` lets both through.
    #[test]
    fn a_non_block_is_refused_by_name() {
        let arcs = [(0, 2), (0, 3), (1, 2)];
        let pg = graph_of(4, &arcs);
        let pgt = transpose_of(4, &arcs);
        let err = fetch_block(&pg, &pgt, 2, true).unwrap_err();
        assert!(err.contains("input 1"), "{err}");
        assert!(fetch_block(&pg, &pgt, 2, false).is_ok(), "clause (iii) is what --no-check skips");

        // K({0,1},{3,4}) and one more arc into 4: every input's successors
        // are the whole of O, so only the transpose side can see it.
        let arcs = [(0, 3), (0, 4), (1, 3), (1, 4), (2, 4)];
        let pg = graph_of(5, &arcs);
        let pgt = transpose_of(5, &arcs);
        let err = fetch_block(&pg, &pgt, 3, true).unwrap_err();
        assert!(err.contains("output 4") && err.contains("predecessor 2"), "{err}");
        assert!(fetch_block(&pg, &pgt, 3, false).is_ok());
    }

    /// U3: a chain of three blocks, every node drawn once, nothing dropped.
    #[test]
    fn a_chain_of_blocks() {
        let scene = scene_of(6, &CHAIN, &[1], &BlockPrune::default(), true);
        let r = &scene.report;
        assert_eq!(r.blocks_drawn, 3);
        assert_eq!(r.nodes_drawn, 6);
        assert_eq!(r.arcs_drawn, 8, "2 + 4 + 2");
        assert_eq!((r.outside_inputs, r.outside_sources), (1, 1));
        assert_eq!((r.arcs_cross, r.arcs_stray, r.arcs_dropped, r.arcs_over_nodes), (0, 0, 0, 0));
        assert_eq!(r.sinks, 1);
        assert_eq!(r.frontier_outputs, 0);
        assert_eq!(scene.nodes.iter().filter(|p| p.graph == 3).count(), 1, "node 3 is placed once");
        assert_eq!(r.blocks_checked, 3);
        assert_eq!(r.blocks_mixed, 0);
        assert!(r.lines().ends_with("frontier: outputs_with_undrawn_consumer=0 sinks=1"));
        assert!(r.lines().contains("dropped=0 over_nodes=0\n"));
    }

    /// U4: a diamond in the quotient: the block both branches feed is
    /// discovered once, and the arc from the other branch is a cross arc.
    #[test]
    fn a_diamond_makes_a_cross_arc() {
        let arcs = [(0, 1), (0, 2), (1, 3), (2, 4), (3, 5), (4, 5)];
        let scene = scene_of(6, &arcs, &[1], &BlockPrune::default(), true);
        let r = &scene.report;
        assert_eq!(r.blocks_drawn, 4);
        assert_eq!(r.arcs_drawn, 6);
        assert_eq!(r.arcs_cross, 1);
        assert_eq!(r.arcs_over_nodes, 0);
        let b3 = scene.blocks.iter().find(|b| b.first_out == 5).unwrap();
        assert_eq!(b3.inputs, vec![3, 4]);
        assert_eq!(b3.parent.map(|(_, o)| o), Some(3), "discovered through 3, since b1 expands before b2");
        let cross = scene.arcs.iter().find(|a| a.kind == ArcKind::Cross).unwrap();
        assert_eq!(
            (scene.nodes[cross.from as usize].graph, scene.nodes[cross.to as usize].graph),
            (4, 5)
        );
    }

    /// U5: a block the budget refused leaves an outside input whose producer
    /// is drawn: the arc between them is stray, and the producer is cut.
    #[test]
    fn a_refused_block_leaves_a_stray_arc() {
        let arcs = [(0, 1), (0, 2), (1, 3), (2, 4), (4, 5), (3, 6), (5, 6)];
        let prune = BlockPrune { max_blocks: 4, ..BlockPrune::default() };
        let scene = scene_of(7, &arcs, &[1], &prune, true);
        let r = &scene.report;
        assert_eq!(r.blocks_drawn, 4);
        assert_eq!(r.blocks_refused_by_budget, 1, "K({{4}},{{5}})");
        assert_eq!(r.arcs_stray, 1);
        assert_eq!(r.outside_from_undrawn, 1, "5, produced by the refused block");
        let stray = scene.arcs.iter().find(|a| a.kind == ArcKind::Stray).unwrap();
        assert_eq!(
            (scene.nodes[stray.from as usize].graph, scene.nodes[stray.to as usize].graph),
            (4, 5)
        );
        let four = scene.nodes.iter().find(|p| p.graph == 4).unwrap();
        assert!(four.outdeg > 0 && !four.consumed, "4 is a frontier: the graph goes on, the page does not");
        assert_eq!(r.frontier_outputs, 1);
        assert_eq!(r.blocks_beyond_depth, 0, "the refused block is counted as refused, not as beyond");
        assert_eq!(r.blocks_mixed, 1, "K({{3,5}},{{6}}) has 3 drawn and 5 outside");
    }

    /// U6: the fanout keeps both ends of a fan and stands an ellipsis for the
    /// middle; the hidden outputs are asked whether they lead anywhere; and a
    /// hidden output the page consumes anyway is un-hidden, the ellipsis
    /// splitting around it.
    #[test]
    fn the_fanout_keeps_both_ends() {
        let arcs: Vec<(usize, usize)> = (1..=9).map(|v| (0, v)).collect();
        let prune = BlockPrune { fanout_out: Some(4), ..BlockPrune::default() };
        let scene = scene_of(10, &arcs, &[1], &prune, true);
        let r = &scene.report;
        assert_eq!(r.blocks_drawn, 1);
        assert_eq!(r.blocks_cut_by_fanout, 1);
        let mut drawn: Vec<usize> = scene
            .nodes
            .iter()
            .filter(|p| matches!(p.role, Role::Output { .. }))
            .map(|p| p.graph)
            .collect();
        drawn.sort_unstable();
        assert_eq!(drawn, [1, 2, 8, 9]);
        assert_eq!(scene.ellipses.len(), 1);
        assert_eq!(scene.ellipses[0].hidden, 5);
        assert_eq!(scene.ellipses[0].slot, 2, "between the head and the tail");
        assert_eq!((r.arcs_drawn, r.arcs_hidden_by_fanout), (4, 5));
        assert_eq!((r.outputs_hidden, r.inputs_hidden), (5, 0));
        assert_eq!(r.hidden_outputs_with_consumer, 0, "every hidden output is a sink");
        assert_eq!(r.blocks_beyond_depth, 0);

        // The same fan, its hidden outputs spent: what the ellipsis stands in
        // front of is counted.
        let mut spent = arcs.clone();
        spent.extend([(4, 10), (5, 11)]);
        let scene = scene_of(12, &spent, &[1], &prune, true);
        assert_eq!(scene.report.hidden_outputs_with_consumer, 2);
        assert_eq!(scene.report.blocks_beyond_depth, 2);

        // A diamond through a hidden output: K({4,9},{10}) is reached through
        // the kept 9, and 4 is then drawn where its producer put it, not as
        // an outside input, with an ellipsis on either side.
        let mut diamond = arcs.clone();
        diamond.extend([(4, 10), (9, 10)]);
        let scene = scene_of(11, &diamond, &[1], &prune, true);
        let r = &scene.report;
        assert_eq!(r.blocks_drawn, 2);
        assert_eq!((r.nodes_drawn, r.outputs_drawn, r.outside_inputs), (7, 6, 1));
        assert_eq!((r.arcs_drawn, r.arcs_hidden_by_fanout), (5 + 2, 4));
        assert_eq!((r.outputs_hidden, r.hidden_outputs_with_consumer), (4, 0));
        assert_eq!(r.blocks_mixed, 0);
        assert_eq!(r.arcs_cross, 0, "4 is an output of the parent block");
        let mut gaps: Vec<(usize, usize)> = scene.ellipses.iter().map(|e| (e.slot, e.hidden)).collect();
        gaps.sort_unstable();
        assert_eq!(gaps, [(2, 1), (4, 3)], "1 2 ... 4 ... 8 9");
        let four = scene.nodes.iter().find(|p| p.graph == 4).unwrap();
        assert!(matches!(four.role, Role::Output { block: 0 }));
        assert_eq!(four.slot, 3);
    }

    /// U7: the geometry of a chain --- rows inside boxes, one unit apart all
    /// the way down, siblings two units apart.
    #[test]
    fn rows_sit_where_the_boxes_put_them() {
        let scene = scene_of(6, &CHAIN, &[1], &BlockPrune::default(), true);
        let at = |g: usize| at(&scene, g);
        // The seed has an input row: input at 0.5, outputs at 1.5.
        assert_eq!(at(0).1, 0.5);
        assert_eq!((at(1).1, at(2).1), (1.5, 1.5));
        assert_eq!((at(2).0 - at(1).0).abs(), 2.0, "siblings two units apart");
        // K({1,2},{3,4}) has no outside inputs: one row, right under.
        assert_eq!((at(3).1, at(4).1), (2.5, 2.5));
        // K({3,4},{5}) likewise.
        assert_eq!(at(5).1, 3.5);
        // The input is centred over its outputs.
        assert_eq!(at(0).0, (at(1).0 + at(2).0) / 2.0);
        assert!(scene.bounds.min_y == 0.0 && scene.bounds.max_y == 4.0);

        // Horizontal: the same numbers on the other axis.
        let scene = scene_of(6, &CHAIN, &[1], &BlockPrune::default(), false);
        assert_eq!(scene.nodes[scene.index[&0] as usize].x, 0.5);
        assert_eq!(scene.nodes[scene.index[&5] as usize].x, 3.5);
    }

    /// U8: a block with drawn and outside inputs --- the shape of the paper's
    /// K({196,259,372},{509}) --- puts its outside input on the row of its
    /// drawn ones, beside the parent, so that every arc of the gadget spans
    /// one row and passes over nothing.
    #[test]
    fn an_outside_input_sits_beside_the_drawn_ones() {
        // K({0},{1,2}); K({1},{3}); K({2},{4,5}); K({3,4,6},{7}), 6 a source.
        let arcs = [(0, 1), (0, 2), (1, 3), (2, 4), (2, 5), (3, 7), (4, 7), (6, 7)];
        for vertical in [true, false] {
            let scene = scene_of(8, &arcs, &[1], &BlockPrune::default(), vertical);
            let r = &scene.report;
            assert_eq!((r.blocks_drawn, r.nodes_drawn, r.arcs_drawn), (4, 8, 8));
            assert_eq!((r.arcs_cross, r.blocks_mixed, r.arcs_over_nodes), (1, 1, 0));
            assert_eq!((r.outside_inputs, r.outside_sources, r.outside_from_undrawn), (2, 2, 0));
            assert_eq!(r.sinks, 2, "5 and 7");
            let t = |g: usize| if vertical { at(&scene, g).1 } else { at(&scene, g).0 };
            let s = |g: usize| if vertical { at(&scene, g).0 } else { at(&scene, g).1 };
            assert_eq!((t(3), t(4), t(6)), (2.5, 2.5, 2.5), "all three inputs on one row");
            assert_eq!(t(7), 3.5);
            assert_eq!(s(6) - s(3), 2.0, "beside its parent's output");
            assert!(s(4) > s(6), "and before the next block");
            assert_eq!(s(7), s(3), "the block hangs under its parent");
        }
    }

    /// A cross arc that skips a row is checked against what it passes over:
    /// here 7 -> 9 crosses the row of 5 and 6 through 6, and 8 -> 9 misses it
    /// by exactly a unit.
    #[test]
    fn an_arc_over_a_node_is_counted() {
        let arcs = [
            (0, 1), (0, 2),
            (1, 3), (1, 4),
            (3, 5), (3, 6), (4, 5), (4, 6),
            (5, 7), (5, 8), (6, 7), (6, 8),
            (2, 9), (7, 9), (8, 9),
        ];
        let scene = scene_of(10, &arcs, &[1], &BlockPrune::default(), true);
        let r = &scene.report;
        assert_eq!((r.blocks_drawn, r.arcs_cross), (5, 2));
        assert_eq!(at(&scene, 9).1, 2.5, "hung under the seed, where 2 is");
        assert_eq!(at(&scene, 7).1, 4.5);
        assert_eq!(r.arcs_over_nodes, 1, "{:?}", scene.nodes);
    }

    /// A seed another seed's output feeds is drawn under it, as the chain it
    /// is, with the arcs between them tree arcs; the seeds say so.
    #[test]
    fn a_seed_reached_by_another_is_drawn_under_it() {
        let pg = graph_of(6, &CHAIN);
        let pgt = transpose_of(6, &CHAIN);
        for depth in [None, Some(0)] {
            let prune = BlockPrune { depth, ..BlockPrune::default() };
            let w = walk(&pg, &pgt, &[3, 1], &prune, Seed::Producing, true).unwrap();
            assert!(w.seeds.iter().any(|s| s.contains("drawn under it")), "{:?}", w.seeds);
            let b = w.blocks.iter().find(|b| b.first_out == 3).unwrap();
            assert_eq!(b.parent.map(|(_, o)| o), Some(1));
            assert_eq!(b.level, 1);
            let scene = lay_out_blocks(&pg, &pgt, w, true).unwrap();
            assert_eq!(scene.report.arcs_cross, 0);
            assert_eq!(at(&scene, 3).1, 2.5, "under K({{0}},{{1,2}})");
            if depth.is_some() {
                assert_eq!(scene.report.blocks_drawn, 2);
                assert_eq!(scene.report.deepest_level, 1);
                assert_eq!(scene.report.blocks_beyond_depth, 1);
            } else {
                assert_eq!(scene.report.blocks_drawn, 3);
                assert_eq!(scene.report.deepest_level, 2);
            }
        }
    }

    /// The scissors that walk in blocks: a depth stops the walk and counts
    /// what lies beyond; a block on the last level is drawn whole.
    #[test]
    fn the_depth_is_in_blocks() {
        let prune = BlockPrune { depth: Some(0), ..BlockPrune::default() };
        let scene = scene_of(6, &CHAIN, &[1], &prune, true);
        let r = &scene.report;
        assert_eq!(r.blocks_drawn, 1);
        assert_eq!(r.frontier_outputs, 2, "1 and 2 lead on");
        assert_eq!(r.blocks_beyond_depth, 1, "both to the same block");
        assert_eq!(r.arcs_drawn, 2);
        assert_eq!(r.arcs_dropped, 0);
        assert!(r.lines().contains("dropped=0"));
        assert!(r.lines().starts_with("blocks: drawn=1 deepest_level=0"));
    }

    /// Two roots in one block draw it once; a seed the budget cannot seat is
    /// an error rather than a smaller picture.
    #[test]
    fn several_roots() {
        let arcs = [(0, 1), (0, 2), (3, 4)];
        let pg = graph_of(5, &arcs);
        let pgt = transpose_of(5, &arcs);
        let w = walk(&pg, &pgt, &[1, 2, 4], &BlockPrune::default(), Seed::Producing, true).unwrap();
        assert_eq!(w.blocks.len(), 2);
        assert!(w.seeds[1].ends_with("(already drawn)"), "{}", w.seeds[1]);
        let spent = walk(
            &pg,
            &pgt,
            &[1, 4],
            &BlockPrune { max_blocks: 1, ..BlockPrune::default() },
            Seed::Producing,
            true,
        );
        assert!(spent.is_err());
        // Two seeds hang side by side off the added root.
        let scene = lay_out_blocks(&pg, &pgt, w, true).unwrap();
        assert_eq!(scene.report.nodes_drawn, 5);
        assert_eq!(at(&scene, 1).1, at(&scene, 4).1);
    }

    /// The slots of a cut side: the kept entries at their places, a gap for
    /// every hidden run, and un-hiding splits a gap.
    #[test]
    fn slots_of_a_cut_side() {
        let mut k = Kept::of(9, Some(4));
        assert_eq!(k.slots(9), [Slot::Node(0), Slot::Node(1), Slot::Gap(5), Slot::Node(7), Slot::Node(8)]);
        k.keep(9, 4);
        k.keep(9, 4);
        assert_eq!(k.indices(9), [0, 1, 4, 7, 8]);
        assert_eq!(
            k.slots(9),
            [Slot::Node(0), Slot::Node(1), Slot::Gap(2), Slot::Node(4), Slot::Gap(2), Slot::Node(7), Slot::Node(8)]
        );
        assert_eq!((k.count(9), k.hidden(9)), (5, 4));
        let one = Kept::of(5, Some(1));
        assert_eq!(one.slots(5), [Slot::Node(0), Slot::Gap(4)]);
        assert_eq!(Kept::of(3, Some(4)), Kept::All);
        assert_eq!(Kept::All.slots(2), [Slot::Node(0), Slot::Node(1)]);
    }
}
