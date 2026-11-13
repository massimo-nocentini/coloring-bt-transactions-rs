//! # `pick`: the sketches of selected records, from one fold
//!
//! Comparing transactions pairwise needs their sketches side by side, and
//! the sketches of the whole chain are far too many to store (about 3.2 TB of
//! KMV ranks at `k = 1024`).  Which records are worth comparing -- deep ones,
//! a window of blocks, an even spread -- is only known after a `count` run,
//! so selection is a second pass: a file of record indices, and one fold of
//! every requested store that dumps each selected record's sketch as it goes
//! by.  `pairs` then reads the dumps, and can be rerun without refolding.
//!
//! One parse feeds one thread per store (`--config`, `--kmv`, `--l1`, and
//! the exact store with `--exact`) and a thread that writes `sel.tsv`; each
//! store thread walks the sorted selection with a cursor.
//!
//! ## Files in `--out`
//!
//! - `sel.tsv`: `idx rec tx block frontier n_inputs is_coinbase group`, where
//!   `frontier` is the newest block seen up to the record, the universe of
//!   its KMV count ([`crate::kmv::Universe`]).
//! - `kmv.<label>.bin` ([`crate::kmv::SEL_MAGIC`]), `l1.<label>.bin`
//!   ([`crate::l1::SEL_MAGIC`]).
//! - `varopt.<label>.bin` (any `--config`, whatever its variant): the
//!   selected records in the `SKETCH01` format, header and trailer included,
//!   so `decode` reads it as it reads a whole-chain file.
//! - `exact.bin` (`--exact`): `"EXSEL002" | u32 N`, then per record `u32 n |
//!   n x (u32 b, f64 w) | u32 z | z x u32 b` -- every term of the exact
//!   colour, zero coefficients included (the `sets` support and the colour),
//!   then the blocks among them outside the `pos` support, ascending
//!   ([`crate::exact::Exact::off`]; almost always none).

use crate::algebra::{Algebra, Config, Sketcher, Variant};
use crate::chain::snapshot;
use crate::driver::Driver;
use crate::encode::{self, Encoder};
use crate::eval::{write_cost, Cost};
use crate::exact::Exact;
use crate::feed::{self, Batch};
use crate::hash::{Keys, Perm};
use crate::kmv::{self, KmvSpec, KmvStore};
use crate::l1::{self, L1Spec, L1Store};
use crate::oracle::Oracle;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;

pub struct PickOpts {
    pub select: String,
    pub out: String,
    pub limit: u64,
    pub configs: Vec<Config>,
    pub kmv: Vec<KmvSpec>,
    pub l1: Vec<L1Spec>,
    pub exact: bool,
    pub stats: bool,
}

/// The selected record indices, ascending and distinct, and each one's group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub recs: Vec<u64>,
    pub groups: Vec<String>,
}

impl Selection {
    /// One index per line, an optional TAB and group name (default `all`),
    /// `#` comments and blank lines skipped.  Sorted and deduplicated; a
    /// record named twice keeps its first group.
    pub fn parse(text: &str) -> Result<Selection, String> {
        let mut v: Vec<(u64, usize, String)> = Vec::new();
        for (no, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split('\t');
            let rec: u64 = it
                .next()
                .and_then(|x| x.trim().parse().ok())
                .ok_or_else(|| format!("selection line {}: not a record index: {:?}", no + 1, line))?;
            let group = it.next().map(|g| g.trim()).filter(|g| !g.is_empty()).unwrap_or("all").to_string();
            v.push((rec, no, group));
        }
        v.sort_by_key(|x| (x.0, x.1));
        v.dedup_by_key(|x| x.0);
        Ok(Selection { recs: v.iter().map(|x| x.0).collect(), groups: v.into_iter().map(|x| x.2).collect() })
    }

    pub fn load(path: &str) -> io::Result<Selection> {
        let text = std::fs::read_to_string(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
        Selection::parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {}", path, e)))
    }

    pub fn len(&self) -> usize {
        self.recs.len()
    }
}

/// The selection's end check: every index must have been read.
fn all_seen(sel: &Selection, seen: usize, records: u64) -> io::Result<()> {
    if seen < sel.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the selection names record {} but only {} records were read", sel.recs[seen], records),
        ));
    }
    Ok(())
}

/// Fold `alg` over the records, calling `dump` on every selected one in
/// order; answers the store's cost.
fn fold<A: Algebra>(
    alg: A,
    label: String,
    oracle: Option<Arc<Oracle>>,
    rx: Receiver<Arc<Batch>>,
    sel: Arc<Selection>,
    stats: bool,
    mut dump: impl FnMut(&A::C, &A, u64, usize) -> io::Result<()>,
) -> io::Result<Cost> {
    let mut d = Driver::new(alg, oracle, stats.then(|| label.clone()));
    let mut next = 0usize;
    while let Ok(batch) = rx.recv() {
        d.batch(&batch, |rec, tx, c, alg| {
            if next < sel.len() && sel.recs[next] == rec {
                dump(c, alg, rec, tx)?;
                next += 1;
            }
            Ok(())
        })?;
    }
    all_seen(&sel, next, d.records)?;
    let w = d.alg.work();
    let u = d.alg.usage();
    let (busy, records) = (d.busy, d.records);
    d.finish();
    Ok(Cost {
        label,
        busy,
        records,
        peak_entries: u.peak_entries,
        peak_sketches: u.peak_sketches,
        reductions: w.reductions,
        off_by_one: w.off_by_one,
    })
}

/// `sel.tsv`, from the batches alone.
fn sel_thread(rx: Receiver<Arc<Batch>>, sel: Arc<Selection>, path: String) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "idx\trec\ttx\tblock\tfrontier\tn_inputs\tis_coinbase\tgroup")?;
    let mut next = 0usize;
    let mut frontier: Option<usize> = None;
    let mut records = 0u64;
    while let Ok(batch) = rx.recv() {
        for (i, r) in batch.recs.iter().enumerate() {
            let rec = batch.first + i as u64;
            let f = frontier.map_or(r.block, |f| f.max(r.block));
            frontier = Some(f);
            if next < sel.len() && sel.recs[next] == rec {
                let n = batch.inputs_of(i).len();
                writeln!(w, "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", next, rec, r.tx, r.block, f, n, (n == 0) as u8, sel.groups[next])?;
                next += 1;
            }
        }
        records = batch.first + batch.len() as u64;
    }
    all_seen(&sel, next, records)?;
    w.flush()
}

pub fn run(
    opts: PickOpts,
    input: Box<dyn io::Read + Send>,
    oracle: Option<Arc<Oracle>>,
    keys: &HashMap<u64, Arc<Keys>>,
    perms: &HashMap<(u64, u32), Arc<Perm>>,
) -> io::Result<()> {
    crate::count::unique_labels("pick", "--config", opts.configs.iter().map(|c| c.label()))?;
    crate::count::unique_labels("pick", "--kmv", opts.kmv.iter().map(|s| s.label()))?;
    crate::count::unique_labels("pick", "--l1", opts.l1.iter().map(|s| s.label()))?;
    std::fs::create_dir_all(&opts.out)?;
    let sel = Arc::new(Selection::load(&opts.select)?);
    let n = u32::try_from(sel.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "selection too large"))?;
    let mut senders = Vec::new();
    let mut stores: Vec<std::thread::JoinHandle<io::Result<Cost>>> = Vec::new();
    let mut channel = || {
        let (tx, rx) = sync_channel::<Arc<Batch>>(8);
        senders.push(tx);
        rx
    };
    let rx = channel();
    let (s, path) = (sel.clone(), format!("{}/sel.tsv", opts.out));
    let sel_handle = std::thread::Builder::new().name("sel".into()).spawn(move || sel_thread(rx, s, path))?;

    for cfg in &opts.configs {
        let rx = channel();
        let cfg = *cfg;
        let k = if cfg.variant == Variant::Coord { keys.get(&cfg.seed).cloned() } else { None };
        let mut w = BufWriter::with_capacity(1 << 20, File::create(format!("{}/varopt.{}.bin", opts.out, cfg.label()))?);
        let (o, s, stats) = (oracle.clone(), sel.clone(), opts.stats);
        stores.push(std::thread::Builder::new().name(cfg.label()).spawn(move || {
            let mut buf = encode::header(&cfg);
            let mut enc = Encoder::default();
            let (mut p64, mut p32) = (Vec::new(), Vec::new());
            let cost = fold(Sketcher::new(cfg, k), cfg.label(), o, rx, s, stats, |c, alg, _, tx| {
                let m = snapshot(alg, c, &mut p64, &mut p32);
                enc.record(&mut buf, tx as u64, &p32, m);
                w.write_all(&buf)?;
                buf.clear();
                Ok(())
            })?;
            enc.trailer(&mut buf);
            w.write_all(&buf)?;
            w.flush()?;
            Ok(cost)
        })?);
    }
    for spec in &opts.kmv {
        let rx = channel();
        let spec = *spec;
        let perm = perms.get(&(spec.seed, spec.bits)).expect("a permutation per spec").clone();
        let mut w = BufWriter::with_capacity(1 << 20, File::create(format!("{}/kmv.{}.bin", opts.out, spec.label()))?);
        w.write_all(&kmv::sel_header(&spec, n))?;
        let (o, s, stats) = (oracle.clone(), sel.clone(), opts.stats);
        stores.push(std::thread::Builder::new().name(spec.label()).spawn(move || {
            let cost = fold(KmvStore::new(spec, perm), spec.label(), o, rx, s, stats, |c, _, _, _| kmv::sel_record(&mut w, c.view()))?;
            w.flush()?;
            Ok(cost)
        })?);
    }
    for spec in &opts.l1 {
        let rx = channel();
        let spec = *spec;
        let mut w = BufWriter::with_capacity(1 << 20, File::create(format!("{}/l1.{}.bin", opts.out, spec.label()))?);
        w.write_all(&l1::sel_header(&spec, n))?;
        let (o, s, stats) = (oracle.clone(), sel.clone(), opts.stats);
        stores.push(std::thread::Builder::new().name(spec.label()).spawn(move || {
            let cost = fold(L1Store::new(spec), spec.label(), o, rx, s, stats, |c, _, _, _| l1::sel_record(&mut w, c))?;
            w.flush()?;
            Ok(cost)
        })?);
    }
    if opts.exact {
        let rx = channel();
        let mut w = BufWriter::with_capacity(1 << 20, File::create(format!("{}/exact.bin", opts.out))?);
        w.write_all(EXACT_MAGIC)?;
        w.write_all(&n.to_le_bytes())?;
        let (o, s, stats) = (oracle.clone(), sel.clone(), opts.stats);
        stores.push(std::thread::Builder::new().name("exact".into()).spawn(move || {
            let cost = fold(Exact::new(), "exact".into(), o, rx, s, stats, |c, alg, _, _| {
                let t = alg.terms(c);
                w.write_all(&(t.len() as u32).to_le_bytes())?;
                for (b, x) in t {
                    w.write_all(&b.to_le_bytes())?;
                    w.write_all(&x.to_le_bytes())?;
                }
                let off = alg.off(c);
                w.write_all(&(off.len() as u32).to_le_bytes())?;
                for b in off {
                    w.write_all(&b.to_le_bytes())?;
                }
                Ok(())
            })?;
            w.flush()?;
            Ok(cost)
        })?);
    }
    let reader = feed::spawn_reader(input, opts.limit, senders);
    let mut costs = Vec::new();
    let mut first_err = None;
    for h in stores {
        match h.join().expect("a store thread panicked") {
            Ok(c) => costs.push(c),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    let sel_result = sel_handle.join().expect("the selection writer panicked");
    let read = reader.join().expect("the reader panicked");
    if let Some(e) = first_err {
        return Err(e);
    }
    sel_result?;
    let records = read?;
    write_cost(&format!("{}/cost.tsv", opts.out), &costs)?;
    eprintln!("sketch pick: {} records read, {} selected, {} stores, written to {}", records, sel.len(), costs.len(), opts.out);
    Ok(())
}

/// `exact.bin`'s magic.
pub const EXACT_MAGIC: &[u8; 8] = b"EXSEL002";

/// `exact.bin` read back: each selected record's terms, zeros included, and
/// the blocks among them outside the `pos` support.
pub fn read_exact(mut input: impl io::Read) -> io::Result<Vec<(Vec<(u32, f64)>, Vec<u32>)>> {
    let mut h = [0u8; 12];
    input.read_exact(&mut h)?;
    if &h[..8] != EXACT_MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not an EXSEL002 file"));
    }
    let n = u32::from_le_bytes(h[8..12].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(n);
    let mut w4 = [0u8; 4];
    let mut w8 = [0u8; 8];
    for _ in 0..n {
        input.read_exact(&mut w4)?;
        let len = u32::from_le_bytes(w4) as usize;
        let mut v = Vec::with_capacity(len);
        for _ in 0..len {
            input.read_exact(&mut w4)?;
            input.read_exact(&mut w8)?;
            v.push((u32::from_le_bytes(w4), f64::from_le_bytes(w8)));
        }
        input.read_exact(&mut w4)?;
        let z = u32::from_le_bytes(w4) as usize;
        if z > len {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "more blocks outside pos than terms"));
        }
        let mut off = Vec::with_capacity(z);
        for _ in 0..z {
            input.read_exact(&mut w4)?;
            off.push(u32::from_le_bytes(w4));
        }
        out.push((v, off));
    }
    Ok(out)
}

/// `sel.tsv` read back.
#[derive(Clone, Debug, PartialEq)]
pub struct SelRow {
    pub idx: usize,
    pub rec: u64,
    pub tx: u64,
    pub block: u32,
    pub frontier: u32,
    pub group: String,
}

pub fn read_sel_tsv(path: &str) -> io::Result<Vec<SelRow>> {
    let f = BufReader::new(File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?);
    let bad = |l: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{}: bad line {:?}", path, l));
    let mut out = Vec::new();
    for line in f.lines().skip(1) {
        let line = line?;
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() < 8 {
            return Err(bad(&line));
        }
        let p = |i: usize| c[i].parse::<u64>().map_err(|_| bad(&line));
        out.push(SelRow {
            idx: p(0)? as usize,
            rec: p(1)?,
            tx: p(2)?,
            block: p(3)? as u32,
            frontier: p(4)? as u32,
            group: c[7].to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_parse_sort_and_dedup() {
        let s = Selection::parse("# a comment\n5\tdeep\n2\n\n5\tother\n9\twindow # trailing\n").unwrap();
        assert_eq!(s.recs, vec![2, 5, 9]);
        assert_eq!(s.groups, vec!["all", "deep", "window"]);
        assert!(Selection::parse("x\n").is_err());
    }
}
