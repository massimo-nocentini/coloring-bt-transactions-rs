//! # Transactions, coloured, in a window
//!
//! [`main`](../coloring_bt_transactions/index.html) gives every transaction a
//! **colour**: the set of blocks its coins descend from.  It then prints that
//! set, or draws it as a row of pixels, and either way what one gets back is one
//! transaction per *line* — a picture in which the shape of the chain, who spent
//! whom, is nowhere.
//!
//! This is the other picture.  The records make a tree — a transaction hangs off
//! the one it spends — the non-layered tidy trees algorithm (van der Ploeg 2014)
//! places it, and every node is drawn in a colour worked out from the colour
//! `main` would have printed for it.  The window, the camera and the quadtree
//! under them are `tree-view`'s, in [`viewer`]; what is new here is the reading,
//! the tree, and the ink.
//!
//! ```text
//! tx-view <records-file> [--limit <n>|all] [--width <px>] [--height <px>]
//! ```
//!
//! Built only with the `gui` feature, since GTK is a C library:
//!
//! ```text
//! cargo run --release --features gui --bin tx-view -- <records-file>
//! ```
//!
//! A file rather than standard input, because the whole of it is read before the
//! window opens — there is no drawing to be made of a stream that has not ended
//! — and naming it lets the title bar say what is being looked at.
//!
//! # The tree the records already hold
//!
//! Each record lists the transactions its inputs spend, and those are its
//! parents: nothing else has to be built to know the shape.  One forward pass is
//! enough because an input cannot spend a transaction that has not happened yet,
//! so a parent is always a record already read, and a depth-first walk of what
//! that pass built — the one [`Scene`] does — is the drawing's node order.
//!
//! A transaction may spend several, though, and a tree gives a node one parent.
//! The **first input's** is the one drawn; the rest are counted and reported, the
//! way [`forest`] reports the arcs it cannot draw.  What the picture loses by it
//! is real: a node's subtree is what descends from it *through first inputs*, not
//! everything its coins reached.  What the picture keeps is that the colour is
//! still the true one — the colouring reads every input, whatever the tree does
//! with them.
//!
//! A transaction with no inputs at all is a coinbase, and coinbases are the roots.
//!
//! # From a set of blocks to one colour
//!
//! A colour is a set of block ids, and a node is one circle: the set has to
//! become three numbers.  Two things about it are worth seeing from across a
//! drawing, so those are the two the circle carries.
//!
//! - **Hue is the oldest block in the set** — where the coins came from.  The
//!   ramp runs across the block ids the file covers, so transactions whose coins
//!   go back to the same early block share a hue, however far apart they are in
//!   the tree.
//! - **Paleness is how many blocks are in the set** — how mixed the coins are.
//!   A coinbase names one block and is drawn in full colour; something that has
//!   gathered coins from thousands of blocks is nearly grey.  Mixing is what the
//!   chain does to a colour over time, and this is what it looks like.
//!
//! The pair is quantised into a few hundred buckets ([`HUES`] by [`MIXES`]) and
//! each node remembers its bucket, which is what lets a frame drawing a hundred
//! thousand circles cost a few hundred fills rather than a hundred thousand —
//! see [`viewer`].
//!
//! What is *lost* is everything between the oldest and the newest block of a
//! colour, which for a heavily mixed transaction is nearly all of it.  Clicking
//! one says how many blocks it really names, and which the extremes are; the
//! whole set is what `main --jp2` draws, and a window of circles cannot.
//!
//! # What it costs to read a file
//!
//! The colouring is `main`'s, unweighted, over [`colorset::SetStore`] — the fast
//! backend — and it keeps `main`'s memory behaviour: a transaction's colour is
//! dropped as soon as its last unspent output is spent, so the colours in flight
//! track the UTXO set rather than the chain.  The tree cannot be dropped that
//! way, since every node stays in the drawing, so a run costs a node per record
//! either way: the arena for the layout, and then the [`Scene`] and its index.

use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io;
use std::process::ExitCode;

use non_layered_tidy_trees::{Arena, NodeId};

#[path = "tree/camera.rs"]
mod camera;
// For the shape of a node and the layout: `tree-view` and this draw the same
// trees the same way because they ask this file for both.
#[allow(dead_code)]
#[path = "tree/forest.rs"]
mod forest;
#[path = "tree/quadtree.rs"]
mod quadtree;
#[path = "tree/scene.rs"]
mod scene;
#[path = "tree/viewer.rs"]
mod viewer;

// The colouring itself, straight out of the main binary rather than reimplemented
// beside it: `src/*.rs` belong to that binary, so they are reached by `#[path]`
// exactly as the drawing modules above are.
//
// Two lints go off over that, and both are about this being a *part* of the
// driver rather than the whole of it.  Only some of what these modules offer is
// wanted here, so the rest reads as dead; and they document themselves by
// pointing at `crate::weighted`, which is the one backend a drawing has no use
// for -- `--weighted` answers how *much* of a transaction's coins came from each
// block, and a circle has nowhere to put that.  The prose is worth more where it
// is than the link is here.
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../colorset.rs"]
mod colorset;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../poly.rs"]
mod poly;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../sexp.rs"]
mod sexp;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../simd.rs"]
mod simd;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../store.rs"]
mod store;

use colorset::{Set, SetStore};
use forest::{DIAMETER, SUBTREE_MARGIN};
use scene::Scene;
use store::ColorStore;
use viewer::{crowding, Paint, Rgb, View, DEFAULT_HEIGHT, DEFAULT_WIDTH};

const USAGE: &str =
    "usage: tx-view <records-file> [--limit <n>|all] [--width <px>] [--height <px>]";

/// How many hues the ramp is cut into.
///
/// Enough that neighbouring blocks are told apart on a ramp 200 pixels long, few
/// enough that the palette is a small table and a frame is a few hundred fills.
pub const HUES: usize = 96;

/// How many degrees of mixing a colour is sorted into: one per doubling of the
/// number of blocks it names, up to sixteen and over.
pub const MIXES: usize = 5;

/// What is kept of one transaction: enough to draw it, and enough to say what it
/// was when it is clicked on.
///
/// Five numbers rather than the colour itself, and deliberately: the colours of
/// a million transactions are what `main`'s UTXO bookkeeping exists to avoid
/// holding all at once, and a drawing that held them would be the one thing the
/// program is careful not to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tx {
    /// The transaction's own id, as the record gives it.
    id: u64,
    /// The block it was mined in.
    block: u32,
    /// The smallest block id in its colour: where its oldest coin came from.
    oldest: u32,
    /// The largest, which for a coinbase is the same block.
    newest: u32,
    /// How many blocks the colour names.
    blocks: u32,
}

/// A transaction that still has outputs nobody has spent.
///
/// The colour and the tree node are held together because they go out of reach
/// together: a transaction whose last output has been spent can never be named
/// by a later record, so neither its colour nor its place in the tree is ever
/// wanted again.  That is `main`'s bookkeeping, doing one more job.
struct Live {
    colour: Set,
    unspent: usize,
    node: NodeId,
}

/// What one pass over the records leaves behind.
struct Records {
    arena: Arena,
    root: NodeId,
    /// Coinbases: transactions with nothing to hang from.
    roots: usize,
    /// Whether a root standing for no transaction was added, to give the layout
    /// the single root it needs.
    synthetic_root: bool,
    /// Inputs past the first, whose ancestry the tree cannot show.
    dropped: u64,
    /// One per record, in the order they were read.  A node's record is
    /// [`scene::Placed::graph`], which is where this is indexed from.
    txs: Vec<Tx>,
}

impl Records {
    /// The range of block ids the colours name, which is what the hue ramp runs
    /// across.  `(0, 0)` when there are no records.
    fn span(&self) -> (u32, u32) {
        let mut lo = u32::MAX;
        let mut hi = 0;
        for tx in &self.txs {
            lo = lo.min(tx.oldest);
            hi = hi.max(tx.oldest);
        }
        if lo > hi {
            (0, 0)
        } else {
            (lo, hi)
        }
    }

    /// What a run has to say about the file it just read, for stderr.
    fn summary(&self) -> String {
        let mut out = format!(
            "{} transactions, {} coinbase root(s){}",
            self.txs.len(),
            self.roots,
            if self.synthetic_root {
                ", one added root standing for no transaction"
            } else {
                ""
            }
        );
        if self.dropped > 0 {
            out.push_str(&format!(
                "\n{} input(s) not drawn: a transaction spending several has one parent here",
                self.dropped
            ));
        }
        // The range the hue ramp runs across, in the words the legend uses for
        // it: these are the *oldest* blocks of the colours, not the blocks the
        // transactions were mined in.
        let (lo, hi) = self.span();
        out.push_str(&format!("\noldest block {} to {}, which is the hue ramp", lo, hi));
        out
    }
}

/// The complaint `main` makes about the same thing, in the same words.
fn unknown(tx: usize, previous: usize) -> String {
    format!("transaction {} spends unknown transaction {}", tx, previous)
}

/// Reads at most `limit` records, colouring them and building the tree.
///
/// One pass: the tree is grown from the inputs as they are read, and the colour
/// is the fold `main::run` does, with the weights left out — this is the
/// unweighted answer, so every coefficient is 1 and a colour is a set of blocks.
fn read(input: impl io::Read, limit: usize) -> Result<Records, String> {
    let mut reader = sexp::Reader::new(input);
    let mut inputs: Vec<sexp::Input> = Vec::new();

    let mut store = SetStore::new();
    let mut live: HashMap<usize, Live> = HashMap::new();

    let mut arena = Arena::new();
    let mut roots: Vec<NodeId> = Vec::new();
    let mut txs: Vec<Tx> = Vec::new();
    let mut dropped = 0u64;

    while txs.len() < limit {
        let record = match reader.next_record(&mut inputs).map_err(|e| e.to_string())? {
            Some(r) => r,
            None => break,
        };

        // The tree first.  Every record is a node, whether or not anything ever
        // spends it, and its parent is the transaction its first input spends.
        let node = arena.add_node(txs.len() + 1, DIAMETER, DIAMETER, SUBTREE_MARGIN, false);
        match inputs.first() {
            None => roots.push(node),
            Some(first) => {
                let parent = live
                    .get(&first.prev_tx_id)
                    .ok_or_else(|| unknown(record.tx_id, first.prev_tx_id))?
                    .node;
                arena.push_child(parent, node);
                dropped += inputs.len() as u64 - 1;
            }
        }

        // Then the colour, which reads every input however few of them the tree
        // could draw.  `foldr`, so right to left, which is the order `main`
        // folds in and so the order in which entries run out of unspent outputs.
        let colour = if inputs.is_empty() {
            // Coinbase: the block that minted it is the whole colour.
            store.singleton(record.block_id)
        } else {
            let mut accumulator: Option<Set> = None;
            for i in (0..inputs.len()).rev() {
                let previous = inputs[i].prev_tx_id;
                let entry = live
                    .get_mut(&previous)
                    .ok_or_else(|| unknown(record.tx_id, previous))?;

                if entry.unspent > 1 {
                    // Others can still reach this colour, so it has to survive
                    // the fold: merge from a borrow.
                    entry.unspent -= 1;
                    let held = &entry.colour;
                    accumulator = Some(match accumulator.take() {
                        None => store.share(held),
                        Some(acc) => {
                            let combined = store.combine(held, 1.0, &acc, 1.0);
                            store.release(acc);
                            combined
                        }
                    });
                } else {
                    // The last unspent output is being spent right now, so
                    // nobody can reach this colour again and it may be taken
                    // outright — along with the entry, since nothing can name
                    // that transaction as a parent any more either.
                    let taken = live.remove(&previous).expect("just looked it up");
                    accumulator = Some(match accumulator.take() {
                        None => taken.colour,
                        Some(acc) => {
                            let combined = store.combine(&taken.colour, 1.0, &acc, 1.0);
                            store.release(acc);
                            store.release(taken.colour);
                            combined
                        }
                    });
                }
            }
            accumulator.expect("inputs is non-empty, so the fold ran at least once")
        };

        let (mut oldest, mut newest, mut blocks) = (u32::MAX, 0u32, 0u32);
        store.for_each_term(&colour, |block, _| {
            let block = block as u32;
            oldest = oldest.min(block);
            newest = newest.max(block);
            blocks += 1;
        });
        let block = u32::try_from(record.block_id).map_err(|_| {
            format!("block id {} does not fit in a u32", record.block_id)
        })?;
        if blocks == 0 {
            // No colour to speak of, which the fold above cannot produce — every
            // colour starts as a block and union never empties one.  Drawn as
            // its own block rather than left to sort into a bucket that does not
            // exist.
            oldest = block;
            newest = block;
        }
        txs.push(Tx {
            id: record.tx_id as u64,
            block,
            oldest,
            newest,
            blocks,
        });

        if record.outputs > 0 {
            let was = live.insert(
                record.tx_id,
                Live { colour, unspent: record.outputs, node },
            );
            if let Some(displaced) = was {
                store.release(displaced.colour);
            }
        } else {
            // Nothing to spend, so nothing will ever name it: it is a leaf of
            // the tree and its colour is finished with.
            store.release(colour);
        }
    }

    if txs.is_empty() {
        return Err("the file has no transactions to draw".to_string());
    }

    let synthetic_root = roots.len() > 1;
    let root = if synthetic_root {
        // Zero by zero, so that it occupies neither a column of depth nor a slot
        // of breadth, exactly as `forest` explains.
        let r = arena.add_node(0, 0.0, 0.0, 0.0, true);
        arena.set_children(r, &roots);
        r
    } else if let Some(&only) = roots.first() {
        only
    } else {
        // Every record spent one already read, and none was a coinbase: the file
        // starts inside a chain whose head is not in it.  Unreachable, since the
        // first record has nothing before it to spend.
        return Err("no coinbase to hang the tree from".to_string());
    };

    Ok(Records {
        arena,
        root,
        roots: roots.len(),
        synthetic_root,
        dropped,
        txs,
    })
}

/// Hue and saturation, as the module's third section describes them.
struct Colours {
    /// The palette, [`HUES`] hues by [`MIXES`] degrees of mixing.
    palette: Vec<Rgb>,
    /// Which bucket of it each node is drawn in, by *scene* index — the frame
    /// asks this per visible node, so it is worth having the permutation applied
    /// once here rather than a lookup through the record index every frame.
    bucket: Vec<u16>,
    /// What was read, by record index, for the panel.
    txs: Vec<Tx>,
    /// The block ids the ramp runs between.
    span: (u32, u32),
}

impl Colours {
    fn of(scene: &Scene, txs: Vec<Tx>, span: (u32, u32)) -> Colours {
        let palette = (0..HUES * MIXES).map(|b| tone(b / MIXES, b % MIXES)).collect();
        let bucket = (0..scene.len() as u32)
            .map(|i| slot(&txs[scene.node(i).graph as usize], span) as u16)
            .collect();
        Colours { palette, bucket, txs, span }
    }

    fn rgb(&self, i: u32) -> Rgb {
        self.palette[self.bucket[i as usize] as usize]
    }
}

/// Which bucket of the palette a transaction is drawn in.
fn slot(tx: &Tx, (lo, hi): (u32, u32)) -> usize {
    let span = (hi - lo).max(1) as f64;
    let hue = ((tx.oldest.saturating_sub(lo) as f64 / span) * (HUES - 1) as f64).round() as usize;
    // One level per doubling: 1 block, 2 or 3, 4 to 7, 8 to 15, and everything
    // from 16 up in the last.
    let mix = (tx.blocks.max(1).ilog2() as usize).min(MIXES - 1);
    hue.min(HUES - 1) * MIXES + mix
}

/// The colour of hue slot `h` at mixing level `m`.
///
/// The ramp stops short of the whole wheel: gone right round, the oldest block
/// and the newest would both be red and the picture would say they were alike.
/// Mixing takes the colour out and lets the light up, so a thoroughly mixed
/// transaction is a pale grey with a memory of a hue in it.
fn tone(h: usize, m: usize) -> Rgb {
    const SATURATION: [f64; MIXES] = [0.95, 0.78, 0.58, 0.38, 0.20];
    const VALUE: [f64; MIXES] = [0.80, 0.82, 0.84, 0.86, 0.88];
    hsv(330.0 * h as f64 / (HUES - 1) as f64, SATURATION[m], VALUE[m])
}

/// Hue in degrees, saturation and value in `[0, 1]`, as red, green and blue.
fn hsv(h: f64, s: f64, v: f64) -> Rgb {
    let h = (h / 60.0).rem_euclid(6.0);
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as usize {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (r + m, g + m, b + m)
}

impl Paint for Colours {
    fn buckets(&self) -> usize {
        self.palette.len()
    }

    fn bucket(&self, _scene: &Scene, i: u32) -> usize {
        self.bucket[i as usize] as usize
    }

    fn colour(&self, bucket: usize) -> Rgb {
        self.palette[bucket]
    }

    /// A square standing for a crowd of transactions takes the average colour of
    /// a few of them, dimmed by how many there are.
    ///
    /// Sampled rather than averaged over the crowd: eight lookups a square keeps
    /// a zoomed-out frame costing what the window costs, which is the whole
    /// reason the crowd was summarised.  The dimming is what the grey drawing
    /// does with the count, kept because a dense trunk and a thin fringe are
    /// worth telling apart even once both have a hue.
    fn cluster(&self, nodes: &[u32]) -> Rgb {
        const SAMPLES: usize = 8;
        let step = (nodes.len() / SAMPLES).max(1);

        let (mut r, mut g, mut b, mut n) = (0.0, 0.0, 0.0, 0.0);
        for &i in nodes.iter().step_by(step).take(SAMPLES) {
            let c = self.rgb(i);
            r += c.0;
            g += c.1;
            b += c.2;
            n += 1.0;
        }
        if n == 0.0 {
            return (1.0, 1.0, 1.0);
        }

        // `crowding` is a shade from 0.72 for one node down to 0 for thousands;
        // as a factor on a colour that is full strength alone and a little under
        // half in the thick of it.
        let dim = 0.45 + 0.55 * crowding(nodes.len() as u32) / 0.72;
        (r / n * dim, g / n * dim, b / n * dim)
    }

    fn describe(&self, scene: &Scene, chosen: Option<u32>) -> Vec<String> {
        let Some(i) = chosen else {
            // Two lines either way, so that the panel does not change height
            // under the pointer.
            return vec!["nothing selected — click a transaction".to_string(), String::new()];
        };
        let tx = self.txs[scene.node(i).graph as usize];
        let subtree = scene.subtree(i);
        vec![
            format!(
                "tx {} in block {} — {} spending below it",
                tx.id,
                tx.block,
                subtree.end - subtree.start - 1
            ),
            format!(
                "colour: {} block(s), oldest {}, newest {}",
                tx.blocks, tx.oldest, tx.newest
            ),
        ]
    }

    /// The ramp itself, in the corner, so that a hue can be read as a block.
    fn overlay(&self, cr: &gtk4::cairo::Context, width: f64, _height: f64) {
        use gtk4::cairo;
        use viewer::ink;

        let (x, y) = (10.0, 10.0);
        let (w, h) = (220.0_f64.min(width - 20.0), 10.0);
        if w <= 0.0 {
            return;
        }

        cr.set_source_rgba(1.0, 1.0, 1.0, 0.86);
        cr.rectangle(x - 6.0, y - 6.0, w + 12.0, h + 26.0);
        ink(cr.fill());

        // The fully saturated end of every hue, which is what an unmixed
        // transaction of that block is drawn in.
        cr.set_antialias(cairo::Antialias::None);
        for k in 0..HUES {
            let c = self.palette[k * MIXES];
            cr.set_source_rgb(c.0, c.1, c.2);
            cr.rectangle(x + w * k as f64 / HUES as f64, y, w / HUES as f64 + 1.0, h);
            ink(cr.fill());
        }
        cr.set_antialias(cairo::Antialias::Default);

        cr.select_font_face("monospace", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(11.0);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.move_to(x, y + h + 13.0);
        ink(cr.show_text(&format!(
            "oldest block {}..{} · pale = mixed",
            self.span.0, self.span.1
        )));
    }
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = env::args().skip(1).collect();

    let mut files: Vec<&str> = Vec::new();
    let mut width = DEFAULT_WIDTH;
    let mut height = DEFAULT_HEIGHT;
    let mut limit = usize::MAX;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            flag @ ("--width" | "--height") => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("{flag} wants a number\n{USAGE}"))?;
                let px = v
                    .parse::<i32>()
                    .ok()
                    .filter(|&px| px > 0)
                    .ok_or_else(|| format!("{flag} {v}: a window needs a positive size"))?;
                if flag == "--width" {
                    width = px;
                } else {
                    height = px;
                }
            }
            "--limit" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--limit wants a count\n{USAGE}"))?;
                limit = match v.as_str() {
                    "all" => usize::MAX,
                    n => n
                        .parse::<usize>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or_else(|| format!("--limit {n}: a drawing needs a record or more"))?,
                };
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other if other.starts_with('-') => return Err(format!("unknown flag {other}\n{USAGE}")),
            other => files.push(other),
        }
        i += 1;
    }

    let [path] = files.as_slice() else {
        return Err(format!("expected one records file, got {}\n{USAGE}", files.len()));
    };

    // All of this happens before the window opens: a file of any size takes long
    // enough to read and lay out that an empty window would look like a hung
    // one, and stderr can say what is going on where a window not yet drawn
    // cannot.
    eprintln!("reading {path}");
    let file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let read = read(file, limit).map_err(|e| format!("{path}: {e}"))?;
    eprintln!("{}", read.summary());

    eprintln!("laying out");
    let span = read.span();
    let txs = read.txs;
    let arena = forest::lay_out(read.arena, read.root);

    eprintln!("indexing");
    let scene = Scene::of(&arena, read.root)?;
    // The arena is the layout's working shape and much the larger of the two;
    // nothing after this point wants it, so it goes before the window opens.
    drop(arena);
    eprintln!(
        "{} nodes in {} cells, {} deep",
        scene.len(),
        scene.index().cells(),
        scene.index().depth()
    );

    let colours = Colours::of(&scene, txs, span);
    let title = format!("{path} — {} transactions", scene.len());
    viewer::show(
        View::new(scene, colours),
        "it.unifi.coloring-bt-transactions.tx-view",
        &title,
        width,
        height,
        "tx-view",
    )
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use viewer::frame;

    /// One record: the block it is in, its id, what its inputs spend, and how
    /// many outputs it has.
    struct R(usize, usize, Vec<usize>, usize);

    /// The same, spelled the way the tables below read best.
    fn r(block: usize, tx: usize, spends: &[usize], outputs: usize) -> R {
        R(block, tx, spends.to_vec(), outputs)
    }

    /// The records as a file of them, in the format [`sexp`] reads: a header of
    /// seven fields with the block at 1 and the id at 2, then the inputs, then
    /// the outputs.
    fn file(records: &[R]) -> Vec<u8> {
        let mut out = String::new();
        for R(block, tx, spends, outputs) in records {
            let spends: &[usize] = spends;
            out.push_str(&format!("((0 {block} {tx} 0 0 0 0) ("));
            for previous in spends {
                // (address amount previous-tx vout)
                out.push_str(&format!("(0 1 {previous} 0)"));
            }
            out.push_str(") (");
            for _ in 0..*outputs {
                out.push_str("(0 1 0)");
            }
            out.push_str("))\n");
        }
        out.into_bytes()
    }

    /// A drawing of `records`, laid out and indexed, framed in a window
    /// `width` by `height`.  The whole of what the program does before GTK.
    fn viewing(records: &[R], width: f64, height: f64) -> View<Colours> {
        let read = read(&file(records)[..], usize::MAX).unwrap();
        let span = read.span();
        let txs = read.txs;
        let arena = forest::lay_out(read.arena, read.root);
        let scene = Scene::of(&arena, read.root).unwrap();
        let colours = Colours::of(&scene, txs, span);

        let mut view = View::new(scene, colours);
        view.framing(width, height);
        view
    }

    /// What `read` said about a file it would not draw.
    ///
    /// `Result::unwrap_err` wants the other side to be printable and a
    /// [`Records`] holds an arena, so the refusal is taken by hand.
    fn refused(records: &[R]) -> String {
        match read(&file(records)[..], usize::MAX) {
            Err(complaint) => complaint,
            Ok(read) => panic!("{} records drawn, and none of them should have been", read.txs.len()),
        }
    }

    /// Two coinbases, and a chain of spending that gathers both.
    ///
    /// ```text
    /// tx 10 (block 0) ── tx 12 ── tx 13
    /// tx 11 (block 1) ──────┘
    /// ```
    fn mixing() -> Vec<R> {
        vec![
            r(0, 10, &[], 1),
            r(1, 11, &[], 1),
            r(2, 12, &[10, 11], 1),
            r(3, 13, &[12], 1),
        ]
    }

    /// The tree is the one the inputs describe, in the order the records came.
    #[test]
    fn the_records_make_the_tree() {
        let read = read(&file(&mixing())[..], usize::MAX).unwrap();

        assert_eq!(read.roots, 2, "two coinbases");
        assert!(read.synthetic_root, "which need a node to hang from");
        assert_eq!(read.dropped, 1, "tx 12 spends two and is drawn under one");

        let arena = forest::lay_out(read.arena, read.root);
        let scene = Scene::of(&arena, read.root).unwrap();

        assert_eq!(scene.len(), 4, "the added root is not drawn");
        // Pre-order: record 0 and everything under it, then record 1.
        let order: Vec<u32> = (0..4).map(|i| scene.node(i).graph).collect();
        assert_eq!(order, [0, 2, 3, 1]);

        assert_eq!(scene.node(0).parent, scene::NO_PARENT, "a coinbase is a root");
        assert_eq!(scene.node(3).parent, scene::NO_PARENT);
        assert_eq!(scene.node(1).parent, 0, "tx 12 hangs off its first input, tx 10");
        assert_eq!(scene.node(2).parent, 1);
        assert_eq!(scene.subtree(0), 0..3, "tx 10 and the two below it");
    }

    /// The colours are the ones `main` prints, read off as three numbers: a
    /// coinbase names its own block, and spending gathers the blocks.
    #[test]
    fn the_colours_are_the_ancestry() {
        let read = read(&file(&mixing())[..], usize::MAX).unwrap();

        let colour = |i: usize| {
            let tx = read.txs[i];
            (tx.oldest, tx.newest, tx.blocks)
        };

        assert_eq!(colour(0), (0, 0, 1), "a coinbase is the block that minted it");
        assert_eq!(colour(1), (1, 1, 1));
        assert_eq!(colour(2), (0, 1, 2), "tx 12 gathers both");
        assert_eq!(colour(3), (0, 1, 2), "and tx 13 inherits the pair");

        // The ramp runs across the *oldest* blocks, and here those are 0 — for
        // everything descending from the first coinbase — and 1, for the second
        // one alone.
        assert_eq!(read.span(), (0, 1));
    }

    /// A transaction whose coins come from one block is drawn in full colour,
    /// and one that has gathered many is drawn pale.
    #[test]
    fn mixing_takes_the_colour_out() {
        let pure = Tx { id: 0, block: 0, oldest: 0, newest: 0, blocks: 1 };
        let mixed = Tx { id: 1, block: 9, oldest: 0, newest: 9, blocks: 40 };

        let span = (0, 9);
        let (pure, mixed) = (slot(&pure, span), slot(&mixed, span));
        assert_eq!(pure % MIXES, 0, "one block is the unmixed end");
        assert_eq!(mixed % MIXES, MIXES - 1, "forty is past the last doubling");
        assert_eq!(pure / MIXES, mixed / MIXES, "and the hue is the same: both start at block 0");

        // Saturation is what separates them, so the paler one is nearer grey.
        let spread = |c: Rgb| {
            let (lo, hi) = (c.0.min(c.1).min(c.2), c.0.max(c.1).max(c.2));
            hi - lo
        };
        assert!(
            spread(tone(pure / MIXES, pure % MIXES)) > spread(tone(mixed / MIXES, mixed % MIXES)),
            "the mixed transaction is not the paler one"
        );
    }

    /// The hue ramp runs across the blocks the file covers: the oldest coins at
    /// one end of it and the newest at the other, in different colours.
    #[test]
    fn the_ramp_runs_across_the_blocks() {
        let span = (0, 100);
        let old = Tx { id: 0, block: 0, oldest: 0, newest: 0, blocks: 1 };
        let new = Tx { id: 1, block: 100, oldest: 100, newest: 100, blocks: 1 };

        assert_eq!(slot(&old, span) / MIXES, 0);
        assert_eq!(slot(&new, span) / MIXES, HUES - 1);
        assert_ne!(tone(0, 0), tone(HUES - 1, 0), "the ramp does not close on itself");

        // A file covering one block has nothing to spread out, and is still
        // drawable rather than a division by zero.
        assert_eq!(slot(&old, (0, 0)) / MIXES, 0);
    }

    /// A drawing of a handful of transactions is circles, and they are coloured
    /// rather than grey.
    #[test]
    fn a_small_drawing_is_coloured_circles() {
        let mut view = viewing(&mixing(), 400.0, 300.0);
        let pixels = frame(&mut view, 400, 300);

        assert_eq!(view.last.nodes, 4, "all four, one by one");
        assert_eq!(view.last.squares, 0);

        // A pixel no two of whose channels agree cannot be part of the panel's
        // black text or the white paper.
        let coloured = pixels
            .iter()
            .filter(|p| {
                let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
                hi - lo > 40
            })
            .count();
        assert!(coloured > 40, "{coloured} coloured pixels is not four circles and a ramp");
    }

    /// Zoomed out, the drawing is squares standing for crowds, and they keep the
    /// colour of what they stand for rather than going grey.
    #[test]
    fn a_large_drawing_keeps_its_colour() {
        // Ten coinbases, each in a block of its own, each with a long chain of
        // spending off it: the shape a run of transactions has, and wide enough
        // in blocks that the drawing has more than one hue in it to lose.
        // Long enough that a quadtree cell is narrower than a couple of pixels
        // well before it is narrow enough to be a leaf, which is the condition
        // for anything to be summarised at all.
        const CHAINS: usize = 10;
        const EACH: usize = 2_000;

        let mut records = Vec::new();
        let mut tx = 1;
        for block in 0..CHAINS {
            records.push(r(block, tx, &[], 1));
            for _ in 1..EACH {
                records.push(r(block, tx + 1, &[tx], 1));
                tx += 1;
            }
            tx += 1;
        }
        let n = CHAINS * EACH;
        let mut view = viewing(&records, 400.0, 300.0);

        let pixels = frame(&mut view, 400, 300);

        assert!(view.last.squares > 0, "something was summarised");
        assert_eq!(
            view.last.summarised as usize + view.last.nodes,
            n,
            "and every transaction is standing for itself somewhere"
        );

        let coloured = pixels
            .iter()
            .filter(|p| {
                let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
                hi - lo > 40
            })
            .count();
        assert!(coloured > 40, "{coloured} coloured pixels: the squares came out grey");
    }

    /// Clicking a transaction says which one it is and what its colour was.
    #[test]
    fn a_click_says_what_the_colour_is() {
        let mut view = viewing(&mixing(), 400.0, 300.0);

        // Scene index 1 is tx 12, the one that gathered both coinbases.
        let [x, y] = view.scene.at(1);
        let chosen = view.scene.pick(x, y, view.scene.radius()).expect("a node is there");
        view.chosen = Some(chosen);

        let lines = view.paint.describe(&view.scene, view.chosen);
        assert!(lines[0].starts_with("tx 12 in block 2 — 1 spending below it"), "{:?}", lines);
        assert!(lines[1].contains("2 block(s), oldest 0, newest 1"), "{:?}", lines);

        // And the frame still draws, with the selection in the selection's own
        // colour rather than the transaction's.
        frame(&mut view, 400, 300);
        assert_eq!(view.last.nodes, 4);
    }

    /// `--limit` stops early, and what it stops at is a drawing like any other.
    #[test]
    fn a_limit_draws_the_first_of_the_file() {
        let read = read(&file(&mixing())[..], 2).unwrap();
        assert_eq!(read.txs.len(), 2);
        assert_eq!(read.roots, 2, "the two coinbases, and nothing spending them");
        assert_eq!(read.dropped, 0);
    }

    /// A file that spends what it never mentioned is refused, in the same words
    /// the driver refuses it in.
    #[test]
    fn spending_the_unknown_is_refused() {
        let records = [r(0, 1, &[], 1), r(1, 2, &[7], 1)];
        let complaint = refused(&records);
        assert_eq!(complaint, "transaction 2 spends unknown transaction 7");

        // Including when the transaction was known and has been spent out: its
        // last output is gone, so nothing can name it again.
        let records = [r(0, 1, &[], 1), r(1, 2, &[1], 1), r(2, 3, &[1], 1)];
        let complaint = refused(&records);
        assert_eq!(complaint, "transaction 3 spends unknown transaction 1");
    }

    /// The colours are the ones the driver would print, checked against a model
    /// that shares no code with the way they are actually worked out.
    ///
    /// `read` computes them the way `main` does — over
    /// [`SetStore`](colorset::SetStore), with a transaction's colour dropped the
    /// moment its last unspent output is spent, so that the working set follows
    /// the UTXO set rather than the chain.  That bookkeeping is the part most
    /// worth doubting: take a colour one input too early and the answer is
    /// quietly wrong rather than a crash.
    ///
    /// So the model here does the opposite of all of it — a `BTreeSet` per
    /// transaction, kept forever, unioned with no weights, no arena and no
    /// sharing — and the two are compared block for block.  It is the
    /// `--rings` against `--sets` check the crate already makes, made once more
    /// where the drawing reads the answer off.
    #[test]
    fn the_colours_agree_with_a_model_that_forgets_nothing() {
        use std::collections::BTreeSet;

        let records = a_chain_of(2_000);

        // The model: every colour, kept for good.
        let mut colours: HashMap<usize, BTreeSet<u32>> = HashMap::new();
        let mut want: Vec<(u32, u32, u32)> = Vec::new();
        for R(block, tx, spends, _) in &records {
            let colour: BTreeSet<u32> = if spends.is_empty() {
                BTreeSet::from([*block as u32])
            } else {
                spends.iter().flat_map(|p| colours[p].iter().copied()).collect()
            };
            want.push((
                *colour.first().expect("a colour names a block"),
                *colour.last().expect("a colour names a block"),
                colour.len() as u32,
            ));
            colours.insert(*tx, colour);
        }

        let read = read(&file(&records)[..], usize::MAX).unwrap();
        let got: Vec<(u32, u32, u32)> =
            read.txs.iter().map(|t| (t.oldest, t.newest, t.blocks)).collect();

        assert_eq!(got.len(), want.len());
        assert_eq!(got, want);

        // And the check is worth something: the records do mix, so the answer is
        // not a column of one-block colours that any bug would also produce.
        let mixed = want.iter().filter(|&&(_, _, blocks)| blocks > 4).count();
        assert!(mixed > records.len() / 10, "{mixed} mixed colours is too few to prove anything");
    }

    /// A file of `n` records: coinbases in blocks of their own, and transactions
    /// spending one, two or three unspent outputs of what came before.
    ///
    /// Deterministic, since a test that draws a different file every run is one
    /// whose failures cannot be looked at.  The generator tracks the unspent
    /// outputs because the driver refuses a file that spends what is used up,
    /// and that refusal is a different test than this one.
    fn a_chain_of(n: usize) -> Vec<R> {
        // A linear congruential generator, which is a line of arithmetic where a
        // dependency would be a dependency.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |bound: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % bound
        };

        let mut records = Vec::with_capacity(n);
        // The outputs nobody has spent, as (transaction, how many are left).
        let mut unspent: Vec<(usize, usize)> = Vec::new();
        let (mut tx, mut block) = (0usize, 0usize);

        while records.len() < n {
            // Every so often a block ends, and the next one is minted.
            tx += 1;
            let outputs = 1 + next(3);
            records.push(r(block, tx, &[], outputs));
            unspent.push((tx, outputs));
            block += 1;

            for _ in 0..1 + next(8) {
                if records.len() >= n || unspent.is_empty() {
                    break;
                }
                let mut spends = Vec::new();
                for _ in 0..1 + next(3) {
                    if unspent.is_empty() {
                        break;
                    }
                    let at = next(unspent.len());
                    let (previous, left) = unspent[at];
                    if left == 1 {
                        unspent.swap_remove(at);
                    } else {
                        unspent[at].1 = left - 1;
                    }
                    // A transaction spending one of its own inputs twice is a
                    // record the driver would read and the model would double
                    // count; the file never has one, so nor does this.
                    if !spends.contains(&previous) {
                        spends.push(previous);
                    }
                }
                tx += 1;
                let outputs = 1 + next(3);
                records.push(r(block - 1, tx, &spends, outputs));
                unspent.push((tx, outputs));
            }
        }
        records
    }

    /// An empty file has no drawing in it, and says so rather than opening a
    /// window on nothing.
    #[test]
    fn an_empty_file_is_refused() {
        assert!(read(&b""[..], usize::MAX).is_err());
    }

    /// A transaction with no outputs is a leaf nothing can spend, and its colour
    /// is finished with the moment it is drawn.
    #[test]
    fn a_transaction_with_no_outputs_is_a_leaf() {
        let records = [r(0, 1, &[], 1), r(1, 2, &[1], 0)];
        let read = read(&file(&records)[..], usize::MAX).unwrap();

        assert_eq!(read.roots, 1, "one coinbase, and the spend hangs off it");
        assert!(!read.synthetic_root);

        let arena = forest::lay_out(read.arena, read.root);
        let scene = Scene::of(&arena, read.root).unwrap();
        assert!(scene.is_leaf(1));
        assert_eq!(read.txs[1].blocks, 1, "it still has the colour of what it spent");
    }
}
