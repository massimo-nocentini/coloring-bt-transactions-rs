//! # `sketch`: every transaction's colour in about `k` terms
//!
//! `--weighted` gives every transaction its exact colour, the distribution
//! `sum_b w_b x^b` over the blocks its coins were minted in, and it cannot run
//! over the whole 2022 chain: colours reach hundreds of thousands of terms and
//! tens of millions are live at once.  `--bands` makes it fit by giving up the
//! block axis (1024 bands of 745 blocks).  This keeps the block axis and gives
//! up exactness instead: each colour is a *sketch* of about `k` terms whose
//! weights are re-chosen so that, on average, it says what the colour says.
//!
//! The recurrence is `run()`'s, unchanged -- `C = sum_i s_i C_i` over the
//! inputs, folded right to left, with the release oracle deciding when a
//! colour dies ([`driver`]).  What changes is the representation and one
//! extra step: after a record's operands have been merged exactly, a colour
//! with more than `k` terms is reduced back to about `k` ([`algebra`]).
//!
//! ## Which reduction
//!
//! Three, chosen at the command line by `--config <variant>:<k>[:...]`:
//!
//! - **`varopt`** (the default and the expected answer) -- VarOpt_k with the
//!   light terms chosen by systematic sampling in block order and a fresh
//!   offset per reduction ([`varopt`]).  Unbiased at every node, mass exactly
//!   one, and an estimate whose CDF never strays from the colour's by more
//!   than `tau ~ 1/k`.  Two of the three designs measured it independently as
//!   the most accurate per byte, with error falling as `1/k`.  Note that it is
//!   a *threshold* sampler: it keeps every term above `tau` and a systematic
//!   sample of the rest, not "the bottom k" of anything.
//! - **`prio`** -- the literal weighted bottom-k sketch, priority sampling with
//!   fresh draws and Horvitz-Thompson weights ([`prio`]).  Its raw weights are
//!   unbiased; what it emits, normalised to sum one, is a consistent ratio
//!   estimator with an `O(1/k)` bias.
//! - **`coord`** -- coordinated bottom-k with exact lower-bound weights
//!   ([`coord`]): the same key for a block everywhere, so sketches agree with
//!   each other; exact on disjoint supports, but with a bias floor that does
//!   not fall with `k` on this DAG.
//!
//! Approaches the designs measured and dropped are not here: coordinated
//! randomness with HT re-estimation (it diverged, masses of 1e10 to 1e28), a
//! global systematic offset (3x worse, systematically), exponential ranks (no
//! different from priority ranks), deterministic top-k (80-110% error), and
//! the additive missing-weight rules for coord (they compound upward).
//!
//! ## Modes
//!
//! ```text
//! sketch emit   --config <cfg> --oracle <path> [--limit <n>|all] [--format bin|text] [--stats]
//!               < records.scm > out
//! sketch chain  --config <cfg> [--config <cfg> ...] --oracle <path> --out <dir>
//!               [--limit <n>|all] [--every 16] [--rows-every 4001] [--bucket 10000000]
//!               [--laplace-ref <zst>] [--moments-ref <zst>] [--bands-ref <zst>] [--stats] < records.scm
//! sketch eval   --config <cfg> [...] --out <dir> [--limit 1500000] [--every 97] [--oracle <path>]
//!               [--check-ref [--laplace-ref <zst>]] [--metric-threads 8] [--force-exact] [--stats] < records.scm
//! sketch check  --out <dir> [--every 16] [--rows-every 4001] [--bucket 10000000]
//!               [--laplace-ref <zst>] [--moments-ref <zst>] [--bands-ref <zst>] < out.bin
//! sketch decode < out.bin > out.txt
//! ```
//!
//! - **`eval`** ([`eval`]) runs the exact store beside the sketches on a
//!   prefix and measures every sketch against the whole colour.
//! - **`chain`** ([`chain`], [`modes`]) runs the whole chain and measures
//!   against the exact summaries on disk -- Laplace values, moments and,
//!   optionally, the 1024-band binned colour of every transaction.
//! - **`emit`** writes every record's estimate ([`encode`]); production is
//!   `sketch emit ... | zstd -T16 -12 > colors.sketch.zst`.
//! - **`check`** recomputes `chain`'s metric columns from what `emit` wrote,
//!   so a stored file can be validated after the fact.
//!
//! Nothing here picks `k`.  The modes write tables of error against cost per
//! config, bucket and support stratum; a person reads them and decides.
//!
//! A config is `<variant>:<k>[:seed=<u64>][:reduce=record|step][:cap=min|harm]
//! [:floor=<n>]` (`cap` and `floor` are coord's), with defaults seed 1, reduce
//! once per record, cap min, floor 0.  The same seed over the same input
//! writes the same bytes; different seeds are independent replicates, and
//! they are what a bias question needs: the rows of one run share sketches
//! and ancestry, so per-row standard errors (`naive_se`, and the batch-means
//! `block_se`) are lower bounds.  Give one config at several seeds in one
//! `eval` or `chain` pass and `seeds.tsv` reports the means across them with
//! an honest SE.
//!
//! Several configs in one `chain` or `eval` run share one parse of the
//! records: a reader thread broadcasts batches to a thread per store
//! ([`feed`]).  `chain` and `emit` want the release oracle -- without it they
//! hold about 4.8x the colours -- and refuse to run without one unless told
//! `--no-oracle`.  `eval` holds the exact colours too, about 43 GB at 1.5M
//! records, and refuses a prefix past 3M without `--force-exact`.
//!
//! ## What it costs
//!
//! A sketch is its entries at 8 bytes each plus 48 bytes of `Rc` box (16 of
//! counts, 32 of [`algebra::Sketch`]); at `k = 256` a full one is 2048 bytes
//! of entries -- exactly a jemalloc size class, so the rare `k + 1` output
//! (counted as `k+-1` in `--stats`) is the only thing that rounds up, to
//! 2560.  The release oracle is 3.1 GB, loaded once and shared.  Each store's
//! map holds one entry per live colour, 25-40 bytes each.
//!
//! Measured with `emit --config varopt:256 | zstd -T16 -12` over the first
//! 100M records of the 2022 chain (2026-11-12): 17 min wall, the fold busy
//! 872 s; 130k records/s at 10-20M, falling to 80-97k at 40-100M; 2.89M
//! distinct sketches and 704M entries at the end, VmHWM 10.3 GB; 51.4 GB
//! written (514 B a record), 16.4 GB after zstd.  The whole chain peaks at
//! about 7.86M distinct live colours (the band store's peak lanes, at 293.8M
//! records), so a `k = 256` store should peak near 16.5 GB of sketches plus
//! its map, and `k = 1024` near four times that.  Those, and the whole-chain
//! wall time, are extrapolations until a whole-chain run measures them.

#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../bands.rs"]
mod bands;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../oracle.rs"]
mod oracle;
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
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../weighted.rs"]
mod weighted;

#[path = "sketch/algebra.rs"]
mod algebra;
#[path = "sketch/chain.rs"]
mod chain;
#[path = "sketch/coord.rs"]
mod coord;
#[path = "sketch/driver.rs"]
mod driver;
#[path = "sketch/encode.rs"]
mod encode;
#[path = "sketch/eval.rs"]
mod eval;
#[path = "sketch/exact.rs"]
mod exact;
#[path = "sketch/feed.rs"]
mod feed;
#[path = "sketch/hash.rs"]
mod hash;
#[path = "sketch/metrics.rs"]
mod metrics;
#[path = "sketch/modes.rs"]
mod modes;
#[path = "sketch/prio.rs"]
mod prio;
#[path = "sketch/varopt.rs"]
mod varopt;

#[cfg(test)]
#[path = "sketch/tests.rs"]
mod tests;

/// The same allocator `src/main.rs` links, for the same reason: a fold's heap
/// is millions of small blocks freed and reallocated at every merge, which is
/// the shape glibc's malloc degrades on.  Without this line the binary would
/// silently run on the system allocator whatever the features say -- a
/// `#[global_allocator]` belongs to the binary that declares it -- and every
/// time and RSS figure in a `cost.tsv` would be measured on the wrong one.
#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use algebra::{Config, Variant};
use hash::Keys;
use modes::{Format, RefPaths};
use std::collections::HashMap;
use std::io;
use std::process::ExitCode;
use std::sync::Arc;

fn allocator() -> &'static str {
    if cfg!(feature = "jemalloc") {
        "jemalloc"
    } else if cfg!(feature = "mimalloc") {
        "mimalloc"
    } else {
        "system"
    }
}

const USAGE: &str = "usage: sketch emit|chain|eval|check|decode [options] < input   (see the module docs)";

#[derive(Debug)]
struct Args {
    mode: String,
    configs: Vec<Config>,
    limit: Option<u64>,
    oracle: Option<String>,
    no_oracle: bool,
    format: Format,
    stats: bool,
    out: Option<String>,
    every: Option<u64>,
    rows_every: u64,
    bucket: u64,
    refs: RefPaths,
    check_ref: bool,
    metric_threads: usize,
    force_exact: bool,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mode = argv.first().cloned().ok_or_else(|| USAGE.to_string())?;
    let mut a = Args {
        mode,
        configs: Vec::new(),
        limit: None,
        oracle: None,
        no_oracle: false,
        format: Format::Bin,
        stats: false,
        out: None,
        every: None,
        rows_every: 4001,
        bucket: 10_000_000,
        refs: RefPaths::default(),
        check_ref: false,
        metric_threads: 8,
        force_exact: false,
    };
    let mut i = 1;
    let value = |i: usize| -> Result<&String, String> {
        argv.get(i + 1).ok_or_else(|| format!("{} needs a value", argv[i]))
    };
    let number = |i: usize| -> Result<u64, String> {
        let v = value(i)?;
        v.parse().map_err(|_| format!("{} wants a number, not {:?}", argv[i], v))
    };
    while i < argv.len() {
        let mut takes = true;
        match argv[i].as_str() {
            "--config" => a.configs.push(Config::parse(value(i)?)?),
            "--limit" => {
                a.limit = Some(if value(i)? == "all" { u64::MAX } else { number(i)? });
            }
            "--oracle" => a.oracle = Some(value(i)?.clone()),
            "--no-oracle" => {
                a.no_oracle = true;
                takes = false;
            }
            "--format" => {
                a.format = match value(i)?.as_str() {
                    "bin" => Format::Bin,
                    "text" => Format::Text,
                    v => return Err(format!("--format bin|text, not {:?}", v)),
                }
            }
            "--stats" => {
                a.stats = true;
                takes = false;
            }
            "--out" => a.out = Some(value(i)?.clone()),
            "--every" => a.every = Some(number(i)?.max(1)),
            "--rows-every" => a.rows_every = number(i)?.max(1),
            "--bucket" => a.bucket = number(i)?.max(1),
            "--laplace-ref" => a.refs.laplace = Some(value(i)?.clone()),
            "--moments-ref" => a.refs.moments = Some(value(i)?.clone()),
            "--bands-ref" => a.refs.bands = Some(value(i)?.clone()),
            "--check-ref" => {
                a.check_ref = true;
                takes = false;
            }
            "--metric-threads" => a.metric_threads = number(i)?.max(1) as usize,
            "--force-exact" => {
                a.force_exact = true;
                takes = false;
            }
            other => return Err(format!("unknown option {:?}\n{}", other, USAGE)),
        }
        i += if takes { 2 } else { 1 };
    }
    Ok(a)
}

fn need_out(a: &Args) -> Result<String, String> {
    a.out.clone().ok_or_else(|| format!("{} needs --out <dir>", a.mode))
}

fn oracle_for(a: &Args, limit: u64, required: bool) -> io::Result<Option<Arc<oracle::Oracle>>> {
    match &a.oracle {
        Some(path) => {
            let expect = (limit != u64::MAX).then_some(limit);
            let o = oracle::Oracle::load(path, expect)
                .map_err(|e| io::Error::new(e.kind(), format!("oracle {}: {}", path, e)))?;
            Ok(Some(Arc::new(o)))
        }
        None if required && !a.no_oracle => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} holds about 4.8x the colours without the release oracle; pass --oracle <path>, or --no-oracle to run anyway",
                a.mode
            ),
        )),
        None => Ok(None),
    }
}

/// One coord key table per distinct coord seed.
fn keys_for(configs: &[Config]) -> HashMap<u64, Arc<Keys>> {
    let mut m = HashMap::new();
    for c in configs.iter().filter(|c| c.variant == Variant::Coord) {
        m.entry(c.seed).or_insert_with(|| Arc::new(Keys::new(c.seed, bands::CHAIN_BLOCKS + 10)));
    }
    m
}

fn stdin() -> Box<dyn io::Read + Send> {
    Box::new(io::stdin())
}

fn stdout() -> Box<dyn io::Write + Send> {
    Box::new(io::stdout())
}

fn run(a: Args) -> io::Result<()> {
    let invalid = |m: String| io::Error::new(io::ErrorKind::InvalidInput, m);
    if a.stats {
        eprintln!("sketch {}: allocator {}", a.mode, allocator());
    }
    let keys = keys_for(&a.configs);
    match a.mode.as_str() {
        "emit" => {
            if a.configs.len() != 1 {
                return Err(invalid("emit takes exactly one --config".into()));
            }
            let limit = a.limit.unwrap_or(u64::MAX);
            let o = oracle_for(&a, limit, true)?;
            modes::emit(a.configs[0], a.format, stdin(), limit, o, &keys, a.stats, stdout())
        }
        "chain" => {
            if a.configs.is_empty() {
                return Err(invalid("chain needs at least one --config".into()));
            }
            let limit = a.limit.unwrap_or(u64::MAX);
            let o = oracle_for(&a, limit, true)?;
            let opts = modes::ChainOpts {
                configs: a.configs.clone(),
                out: need_out(&a).map_err(invalid)?,
                limit,
                every: a.every.unwrap_or(16),
                rows_every: a.rows_every,
                bucket: a.bucket,
                refs: a.refs.clone(),
                stats: a.stats,
            };
            modes::chain(opts, stdin(), o, &keys)
        }
        "eval" => {
            if a.configs.is_empty() {
                return Err(invalid("eval needs at least one --config".into()));
            }
            let limit = a.limit.unwrap_or(1_500_000);
            if limit > 3_000_000 && !a.force_exact {
                return Err(invalid(format!(
                    "eval holds every exact colour, about 43 GB at 1.5M records; --limit {} needs --force-exact",
                    if limit == u64::MAX { "all".to_string() } else { limit.to_string() }
                )));
            }
            let o = oracle_for(&a, limit, false)?;
            let check_ref = a.check_ref.then(|| {
                a.refs.laplace.clone().unwrap_or_else(|| "/data/bitcoin/2022/colors.laplace.zst".to_string())
            });
            let opts = eval::EvalOpts {
                configs: a.configs.clone(),
                out: need_out(&a).map_err(invalid)?,
                limit,
                every: a.every.unwrap_or(97),
                metric_threads: a.metric_threads,
                check_ref,
                stats: a.stats,
            };
            eval::run(opts, stdin(), o, keys)
        }
        "check" => {
            let opts = modes::CheckOpts {
                out: need_out(&a).map_err(invalid)?,
                every: a.every.unwrap_or(16),
                rows_every: a.rows_every,
                bucket: a.bucket,
                refs: a.refs.clone(),
            };
            modes::check(opts, stdin())
        }
        "decode" => modes::decode(stdin(), stdout()),
        other => Err(invalid(format!("unknown mode {:?}\n{}", other, USAGE))),
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sketch: {}", e);
            return ExitCode::FAILURE;
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sketch: {}", e);
            ExitCode::FAILURE
        }
    }
}
