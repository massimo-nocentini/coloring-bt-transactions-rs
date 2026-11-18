//! # `pairs`: every pair of a `pick`, by every measure it dumped
//!
//! Reads a `pick` directory -- `sel.tsv`, and whichever of `kmv.*.bin`,
//! `l1.*.bin`, `varopt.*.bin` and `exact.bin` it holds -- and for each group
//! of the selection computes all `N (N-1) / 2` pairs: the KMV Jaccard,
//! union, intersection and containments ([`crate::kmv::pair_est`]), the L1
//! estimate of the weighted Jaccard ([`crate::l1`]), the varopt plug-in, and
//! the exact values beside them when `exact.bin` is there.
//!
//! The varopt plug-in `J_w = (1 - TV)/(1 + TV)` of two varopt sketches is
//! written only to document why it is not used: two independent sketches of
//! the *same* colour give a mean of 0.963 and a minimum of 0 (their kept
//! blocks are not coordinated), and its bias drifts with depth.  `W1` between
//! two varopt sketches has no such problem and is the "temporal distance".
//!
//! Union counts use the restricted universe of the pair, `N = max(frontier_i,
//! frontier_j) + 1` ([`crate::kmv::Universe`]), answered offline for all
//! pairs at once ([`crate::kmv::below_many`]); the count over `M` is the
//! `n_union_M` column beside it.
//!
//! Work is split by pair index over `--threads` scoped threads and gathered
//! in order, so the output is byte-identical at any thread count.
//!
//! ## Against the binned colours: `--bands`
//!
//! On the whole chain there is no `exact.bin` -- an exact fold of the chain
//! does not fit -- but `colors.bands1024.zst` holds every record's exact
//! colour binned into 1024 bands of 745 blocks, and `pick --bands-ref` cuts
//! the selected records' lines out of it ([`crate::pick`]; the format is
//! [`crate::describe::parse_bands_line`]'s).  Binning two colours into the
//! same bands can only lower their total variation, so the binned TV,
//! `B_tv1024`, is an exact *lower bound* on the TV the L1 projection
//! estimates, and the check is one-sided: for each L1 source at `m`,
//! `TV-hat >= TV1024 - 3 sqrt(2/m) TV-hat` should hold in at least 99% of the
//! pairs, `sqrt(2/m)` being about the RSE of the median estimator
//! ([`crate::l1`]).  It is a check that passes, not a reference: the bound is
//! far from tight when two colours differ mostly inside the bands.  A band
//! line's tx must be the selected record's, or the run fails.
//!
//! ## Outputs, per group
//!
//! - `pairs.<group>.tsv`: `i j rec_i rec_j block_i block_j` and, per source,
//!   its columns prefixed by its label (`kmv-k1024-s1-sets:J`, ...); the
//!   exact ones are `X_*`, the binned one (with `--bands`) `B_tv1024`.
//! - `m.<group>.<label>.<col>.f32`: `N x N` row-major little-endian matrices
//!   of `J` and `cont` (KMV), `jw` (L1), `w1` (varopt) and `X_J`, `X_cont`,
//!   `X_jw`, `X_J_pos`.  `cont` is not symmetric: cell `(a, b)` is the
//!   containment of row `a`'s support in column `b`'s, so above the diagonal
//!   it is the older in the newer.  The diagonal holds the self-value.
//! - `n.<group>.<label>.f32`: the count of every row.
//! - `rows.<group>.tsv`: the row order (selection order) with rec, block and
//!   the counts.  Seriation is the plotting script's business, not this.
//! - `bands-check.tsv` (with `--bands`): per group and L1 source, the pairs,
//!   the median `TV1024`, how many pairs and what share satisfy the bound,
//!   and the median of `TV-hat / TV1024` over the pairs with `TV1024 > 0.01`.
//!   Medians are numpy's ([`metrics::median_mid`]), as `r7_checks.py` took
//!   them.

use crate::encode::Decoder;
use crate::hash::Perm;
use crate::kmv::{self, KmvSpec, Owned, PairRaw};
use crate::l1::{self, Vector};
use crate::metrics;
use crate::pick::{read_exact, read_sel_tsv, SelRow};
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::sync::Arc;

pub struct PairsOpts {
    pub input: String,
    pub out: String,
    pub groups: Vec<String>,
    pub varopt_text: Option<String>,
    /// `rec tx w:band,...` lines of the selected records' binned colours.
    pub bands: Option<String>,
    pub threads: usize,
}

struct KmvSrc {
    label: String,
    spec: KmvSpec,
    perm: Arc<Perm>,
    sk: Vec<Owned>,
}

struct L1Src {
    label: String,
    m: usize,
    v: Vec<Vector>,
}

struct VoSrc {
    label: String,
    /// Per selected record: reduced, and the estimate normalised in f64.
    recs: Vec<(bool, Vec<(u32, f64)>)>,
}

struct ExactRec {
    /// Every block of the `--sets` support, ascending.
    sets: Vec<u32>,
    /// The blocks a path of positive shares reaches, ascending: the `pos`
    /// support (a superset of the nonzero terms; see [`crate::exact`]).
    pos: Vec<u32>,
    /// The colour, normalised, zeros left out.
    w: Vec<(u32, f64)>,
}

/// Every source a `pick` directory holds.
struct Sources {
    sel: Vec<SelRow>,
    kmv: Vec<KmvSrc>,
    l1: Vec<L1Src>,
    vo: Vec<VoSrc>,
    exact: Option<Vec<ExactRec>>,
    /// Per selected record, its exact colour binned and normalised.
    bands: Option<Vec<Vec<f64>>>,
}

fn bad(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

fn normalise(p: impl Iterator<Item = (u32, f64)>) -> Vec<(u32, f64)> {
    let v: Vec<(u32, f64)> = p.filter(|x| x.1 != 0.0).collect();
    let total: f64 = v.iter().map(|x| x.1).sum();
    if total > 0.0 {
        v.into_iter().map(|(b, w)| (b, w / total)).collect()
    } else {
        v
    }
}

/// `decode --select`'s lines: `rec reduced tau tx w:b,...`.
fn read_varopt_text(path: &str, sel: &[SelRow]) -> io::Result<VoSrc> {
    let f = BufReader::new(File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?);
    let mut by_rec = std::collections::HashMap::new();
    for line in f.lines() {
        let line = line?;
        let c: Vec<&str> = line.splitn(5, '\t').collect();
        if c.len() < 4 {
            return Err(bad(format!("{}: bad line {:?}", path, line)));
        }
        let rec: u64 = c[0].parse().map_err(|_| bad(format!("{}: bad record index in {:?}", path, line)))?;
        let reduced = c[1] == "1";
        let mut p = Vec::new();
        if let Some(terms) = c.get(4).filter(|t| !t.is_empty()) {
            for t in terms.split(',') {
                let (w, b) = t.split_once(':').ok_or_else(|| bad(format!("{}: bad term {:?}", path, t)))?;
                p.push((b.parse::<u32>().map_err(|_| bad(t.into()))?, w.parse::<f64>().map_err(|_| bad(t.into()))?));
            }
        }
        p.reverse();
        by_rec.insert(rec, (reduced, normalise(p.into_iter())));
    }
    let recs = sel
        .iter()
        .map(|r| by_rec.remove(&r.rec).ok_or_else(|| bad(format!("{}: no line for record {}", path, r.rec))))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(VoSrc { label: "varopt-text".into(), recs })
}

fn load(opts: &PairsOpts) -> io::Result<Sources> {
    let dir = &opts.input;
    let sel = read_sel_tsv(&format!("{}/sel.tsv", dir))?;
    let n = sel.len();
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut src = Sources { sel, kmv: Vec::new(), l1: Vec::new(), vo: Vec::new(), exact: None, bands: None };
    let open = |name: &str| -> io::Result<BufReader<File>> { Ok(BufReader::with_capacity(1 << 20, File::open(format!("{}/{}", dir, name))?)) };
    for name in &names {
        let Some(stem) = name.strip_suffix(".bin") else { continue };
        if let Some(label) = stem.strip_prefix("kmv.") {
            let (spec, sk) = kmv::read_sel(open(name)?)?;
            if sk.len() != n {
                return Err(bad(format!("{}: {} sketches for {} selected records", name, sk.len(), n)));
            }
            let perm = Arc::new(Perm::new(spec.seed, spec.bits));
            src.kmv.push(KmvSrc { label: label.to_string(), spec, perm, sk });
        } else if let Some(label) = stem.strip_prefix("l1.") {
            let (spec, v) = l1::read_sel(open(name)?)?;
            if v.len() != n {
                return Err(bad(format!("{}: {} vectors for {} selected records", name, v.len(), n)));
            }
            src.l1.push(L1Src { label: label.to_string(), m: spec.m, v });
        } else if let Some(label) = stem.strip_prefix("varopt.") {
            let mut dec = Decoder::new(open(name)?)?;
            let mut r = crate::encode::Decoded::default();
            let mut recs = Vec::with_capacity(n);
            while dec.next(&mut r)? {
                recs.push((r.meta.reduced, normalise(r.p.iter().map(|&(b, w)| (b, w as f64)))));
            }
            if recs.len() != n {
                return Err(bad(format!("{}: {} records for {} selected", name, recs.len(), n)));
            }
            src.vo.push(VoSrc { label: label.to_string(), recs });
        } else if stem == "exact" {
            let terms = read_exact(open(name)?)?;
            if terms.len() != n {
                return Err(bad(format!("{}: {} colours for {} selected records", name, terms.len(), n)));
            }
            src.exact = Some(
                terms
                    .into_iter()
                    .map(|(t, off)| ExactRec {
                        sets: t.iter().map(|x| x.0).collect(),
                        pos: t.iter().map(|x| x.0).filter(|b| off.binary_search(b).is_err()).collect(),
                        w: normalise(t.into_iter()),
                    })
                    .collect(),
            );
        }
    }
    if let Some(p) = &opts.varopt_text {
        src.vo.push(read_varopt_text(p, &src.sel)?);
    }
    if let Some(p) = &opts.bands {
        let mut b = crate::describe::read_bands(p)?;
        let mut v = Vec::with_capacity(n);
        for r in &src.sel {
            let x = b.remove(&r.rec).ok_or_else(|| bad(format!("{}: no line for record {}", p, r.rec)))?;
            if x.tx != r.tx {
                return Err(bad(format!("{}: record {} is tx {} there but tx {} in sel.tsv", p, r.rec, x.tx, r.tx)));
            }
            v.push(x.p);
        }
        src.bands = Some(v);
    }
    Ok(src)
}

/// `|A n B|` and `|A u B|` of two ascending block lists.
fn inter_union<T>(a: &[T], b: &[T], key: impl Fn(&T) -> u32) -> (u64, u64) {
    let (mut i, mut j, mut inter, mut union) = (0, 0, 0u64, 0u64);
    while i < a.len() || j < b.len() {
        let x = a.get(i).map_or(u32::MAX, &key);
        let y = b.get(j).map_or(u32::MAX, &key);
        union += 1;
        if i < a.len() && (j >= b.len() || x < y) {
            i += 1;
        } else if j < b.len() && (i >= a.len() || y < x) {
            j += 1;
        } else {
            inter += 1;
            i += 1;
            j += 1;
        }
    }
    (inter, union)
}

/// `(TV, sum min, sum max)` of two normalised distributions.
fn tv_min_max(p: &[(u32, f64)], q: &[(u32, f64)]) -> (f64, f64, f64) {
    let (mut i, mut j) = (0, 0);
    let (mut tv, mut smin, mut smax) = (0.0f64, 0.0f64, 0.0f64);
    while i < p.len() || j < q.len() {
        let x = p.get(i).map_or(u32::MAX, |e| e.0);
        let y = q.get(j).map_or(u32::MAX, |e| e.0);
        let b = x.min(y);
        let a = if x == b {
            i += 1;
            p[i - 1].1
        } else {
            0.0
        };
        let c = if y == b {
            j += 1;
            q[j - 1].1
        } else {
            0.0
        };
        tv += (a - c).abs();
        smin += a.min(c);
        smax += a.max(c);
    }
    (0.5 * tv, smin, smax)
}

/// The exact columns of a pair.
#[derive(Clone, Copy, Default)]
struct XPair {
    j: f64,
    cont_ab: f64,
    cont_ba: f64,
    tv: f64,
    jw: f64,
    j_pos: f64,
}

fn x_pair(a: &ExactRec, b: &ExactRec) -> XPair {
    let (inter, union) = inter_union(&a.sets, &b.sets, |x| *x);
    let (ip, up) = inter_union(&a.pos, &b.pos, |x| *x);
    let (tv, smin, smax) = tv_min_max(&a.w, &b.w);
    let ratio = |x: u64, y: usize| if y == 0 { f64::NAN } else { x as f64 / y as f64 };
    XPair {
        j: ratio(inter, union as usize),
        cont_ab: ratio(inter, a.sets.len()),
        cont_ba: ratio(inter, b.sets.len()),
        tv,
        jw: if smax > 0.0 { smin / smax } else { f64::NAN },
        j_pos: ratio(ip, up as usize),
    }
}

/// What one pair's parallel phase computes.
struct PairOut {
    kmv: Vec<PairRaw>,
    tv: Vec<f64>,
    /// `(ks, w1/L, TV)` per varopt source.
    vo: Vec<(f64, f64, f64)>,
    x: Option<XPair>,
    /// The binned colours' TV.
    b: Option<f64>,
}

fn one_pair(src: &Sources, a: usize, b: usize) -> PairOut {
    PairOut {
        kmv: src.kmv.iter().map(|s| kmv::pair_raw(s.sk[a].view(), s.sk[b].view(), s.spec.k)).collect(),
        tv: src.l1.iter().map(|s| l1::tv_hat(&s.v[a], &s.v[b])).collect(),
        vo: src
            .vo
            .iter()
            .map(|s| {
                let (p, q) = (&s.recs[a].1, &s.recs[b].1);
                let (ks, w1, _) = metrics::distances(p, q);
                (ks, w1, tv_min_max(p, q).0)
            })
            .collect(),
        x: src.exact.as_ref().map(|e| x_pair(&e[a], &e[b])),
        b: src.bands.as_ref().map(|v| metrics::tv_bins(&v[a], &v[b])),
    }
}

/// An `N x N` matrix of f32, row-major little-endian.
fn write_matrix(path: &str, n: usize, cell: impl Fn(usize, usize) -> f64) -> io::Result<()> {
    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    for a in 0..n {
        for b in 0..n {
            w.write_all(&(cell(a, b) as f32).to_le_bytes())?;
        }
    }
    w.flush()
}

fn write_vector(path: &str, v: &[f64]) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    for x in v {
        w.write_all(&(*x as f32).to_le_bytes())?;
    }
    w.flush()
}

pub fn run(opts: PairsOpts) -> io::Result<()> {
    std::fs::create_dir_all(&opts.out)?;
    let src = load(&opts)?;
    let mut groups: Vec<String> = Vec::new();
    for r in &src.sel {
        if !groups.contains(&r.group) {
            groups.push(r.group.clone());
        }
    }
    if !opts.groups.is_empty() {
        groups.retain(|g| opts.groups.contains(g));
    }
    let threads = opts.threads.max(1);
    let mut checks: Vec<BandsCheck> = Vec::new();
    for g in &groups {
        let rows: Vec<usize> = (0..src.sel.len()).filter(|&i| &src.sel[i].group == g).collect();
        let n = rows.len();
        let pairs: Vec<(u32, u32)> =
            (0..n).flat_map(|a| ((a + 1)..n).map(move |b| (a as u32, b as u32))).collect();
        // The parallel phase: contiguous chunks of the pair list, in order.
        let chunk = pairs.len().div_ceil(threads).max(1);
        let outs: Vec<PairOut> = std::thread::scope(|s| {
            let hs: Vec<_> = pairs
                .chunks(chunk)
                .map(|c| {
                    let (src, rows) = (&src, &rows);
                    s.spawn(move || c.iter().map(|&(a, b)| one_pair(src, rows[a as usize], rows[b as usize])).collect::<Vec<_>>())
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().expect("a pairs thread panicked")).collect()
        });

        // The restricted universes, offline: each row's count and each full
        // union's, per KMV source.
        let frontier = |i: usize| src.sel[rows[i]].frontier as u64;
        let mut n_row: Vec<Vec<f64>> = Vec::new();
        let mut n_union: Vec<Vec<f64>> = Vec::new();
        for (si, s) in src.kmv.iter().enumerate() {
            let k = s.spec.k;
            let mut q: Vec<(u64, u32)> = Vec::new();
            for i in 0..n {
                q.push((frontier(i) + 1, s.sk[rows[i]].view().rk().unwrap_or(0)));
            }
            for (p, o) in pairs.iter().zip(&outs) {
                let u = frontier(p.0 as usize).max(frontier(p.1 as usize)) + 1;
                q.push((u, o.kmv[si].union_rk.unwrap_or(0)));
            }
            let below = kmv::below_many(&s.perm, &q);
            n_row.push((0..n).map(|i| kmv::count(s.sk[rows[i]].view(), k, q[i].0, below[i])).collect());
            n_union.push(
                outs.iter().enumerate().map(|(p, o)| kmv::union_count(&o.kmv[si], k, q[n + p].0, below[n + p])).collect(),
            );
        }

        let f = metrics::fmt;
        let mut w = BufWriter::with_capacity(1 << 22, File::create(format!("{}/pairs.{}.tsv", opts.out, g))?);
        write!(w, "i\tj\trec_i\trec_j\tblock_i\tblock_j")?;
        for s in &src.kmv {
            for c in [
                "n_i", "n_j", "full_i", "full_j", "exact", "J", "se", "n_union", "n_union_M", "inter", "cont_ij", "t_ij",
                "cont_ji", "t_ji", "cont_ij_fb", "cont_ji_fb",
            ] {
                write!(w, "\t{}:{}", s.label, c)?;
            }
        }
        for s in &src.l1 {
            for c in ["tv", "jw", "jw_se"] {
                write!(w, "\t{}:{}", s.label, c)?;
            }
        }
        for s in &src.vo {
            for c in ["ks", "w1", "jw_plugin", "both_exact", "jw_vo_exact"] {
                write!(w, "\t{}:{}", s.label, c)?;
            }
        }
        if src.exact.is_some() {
            write!(w, "\tX_n_i\tX_n_j\tX_J\tX_cont_ij\tX_cont_ji\tX_tv\tX_jw\tX_n_i_pos\tX_n_j_pos\tX_J_pos")?;
        }
        if src.bands.is_some() {
            write!(w, "\tB_tv1024")?;
        }
        writeln!(w)?;
        // Matrices filled as the pairs are written.
        let mut mat_j = vec![vec![0f64; n * n]; src.kmv.len()];
        let mut mat_c = vec![vec![0f64; n * n]; src.kmv.len()];
        let mut mat_jw = vec![vec![0f64; n * n]; src.l1.len()];
        let mut mat_w1 = vec![vec![0f64; n * n]; src.vo.len()];
        let mut mat_x = vec![vec![0f64; n * n]; if src.exact.is_some() { 4 } else { 0 }];
        for (p, (&(a, b), o)) in pairs.iter().zip(&outs).enumerate() {
            let (a, b) = (a as usize, b as usize);
            let (ra, rb) = (&src.sel[rows[a]], &src.sel[rows[b]]);
            write!(w, "{}\t{}\t{}\t{}\t{}\t{}", a, b, ra.rec, rb.rec, ra.block, rb.block)?;
            for (si, s) in src.kmv.iter().enumerate() {
                let k = s.spec.k;
                let raw = &o.kmv[si];
                let e = kmv::pair_est(raw, k, n_union[si][p]);
                let nu_m = kmv::union_count_m(raw, k, s.perm.universe());
                let (na, nb) = (n_row[si][a], n_row[si][b]);
                let (fa, fb) = (s.sk[rows[a]].full, s.sk[rows[b]].full);
                write_cols(
                    &mut w,
                    &[
                        f(na),
                        f(nb),
                        (fa as u8).to_string(),
                        (fb as u8).to_string(),
                        (e.exact as u8).to_string(),
                        f(e.j),
                        f(e.j_se),
                        f(e.n_union),
                        f(nu_m),
                        f(e.inter),
                        f(e.cont_ab),
                        e.t_ab.to_string(),
                        f(e.cont_ba),
                        e.t_ba.to_string(),
                        f(e.inter / na),
                        f(e.inter / nb),
                    ],
                )?;
                mat_j[si][a * n + b] = e.j;
                mat_j[si][b * n + a] = e.j;
                mat_c[si][a * n + b] = e.cont_ab;
                mat_c[si][b * n + a] = e.cont_ba;
            }
            for (si, s) in src.l1.iter().enumerate() {
                let tv = o.tv[si];
                let jw = l1::jw_of_tv(tv);
                write_cols(&mut w, &[f(tv), f(jw), f(l1::jw_se(tv, s.m))])?;
                mat_jw[si][a * n + b] = jw;
                mat_jw[si][b * n + a] = jw;
            }
            for (si, s) in src.vo.iter().enumerate() {
                let (ks, w1, tv) = o.vo[si];
                let jw = (1.0 - tv) / (1.0 + tv);
                let both = !s.recs[rows[a]].0 && !s.recs[rows[b]].0;
                write_cols(
                    &mut w,
                    &[f(ks), f(w1), f(jw), (both as u8).to_string(), if both { f(jw) } else { "NA".into() }],
                )?;
                mat_w1[si][a * n + b] = w1;
                mat_w1[si][b * n + a] = w1;
            }
            if let (Some(x), Some(e)) = (o.x, &src.exact) {
                let (ea, eb) = (&e[rows[a]], &e[rows[b]]);
                write_cols(
                    &mut w,
                    &[
                        ea.sets.len().to_string(),
                        eb.sets.len().to_string(),
                        f(x.j),
                        f(x.cont_ab),
                        f(x.cont_ba),
                        f(x.tv),
                        f(x.jw),
                        ea.pos.len().to_string(),
                        eb.pos.len().to_string(),
                        f(x.j_pos),
                    ],
                )?;
                for (mi, v) in [(0, x.j), (2, x.jw), (3, x.j_pos)] {
                    mat_x[mi][a * n + b] = v;
                    mat_x[mi][b * n + a] = v;
                }
                mat_x[1][a * n + b] = x.cont_ab;
                mat_x[1][b * n + a] = x.cont_ba;
            }
            if let Some(tv) = o.b {
                write!(w, "\t{}", f(tv))?;
            }
            writeln!(w)?;
        }
        w.flush()?;

        let diag = |m: &mut Vec<f64>, v: f64| {
            for i in 0..n {
                m[i * n + i] = v;
            }
        };
        for (si, s) in src.kmv.iter().enumerate() {
            diag(&mut mat_j[si], 1.0);
            diag(&mut mat_c[si], 1.0);
            write_matrix(&format!("{}/m.{}.{}.J.f32", opts.out, g, s.label), n, |a, b| mat_j[si][a * n + b])?;
            write_matrix(&format!("{}/m.{}.{}.cont.f32", opts.out, g, s.label), n, |a, b| mat_c[si][a * n + b])?;
            write_vector(&format!("{}/n.{}.{}.f32", opts.out, g, s.label), &n_row[si])?;
        }
        for (si, s) in src.l1.iter().enumerate() {
            diag(&mut mat_jw[si], 1.0);
            write_matrix(&format!("{}/m.{}.{}.jw.f32", opts.out, g, s.label), n, |a, b| mat_jw[si][a * n + b])?;
        }
        for (si, s) in src.vo.iter().enumerate() {
            write_matrix(&format!("{}/m.{}.{}.w1.f32", opts.out, g, s.label), n, |a, b| mat_w1[si][a * n + b])?;
        }
        if let Some(e) = &src.exact {
            for (mi, name) in ["X_J", "X_cont", "X_jw", "X_J_pos"].iter().enumerate() {
                diag(&mut mat_x[mi], 1.0);
                write_matrix(&format!("{}/m.{}.{}.f32", opts.out, g, name), n, |a, b| mat_x[mi][a * n + b])?;
            }
            let xn: Vec<f64> = rows.iter().map(|&r| e[r].sets.len() as f64).collect();
            write_vector(&format!("{}/n.{}.X_n.f32", opts.out, g), &xn)?;
        }
        let mut w = BufWriter::new(File::create(format!("{}/rows.{}.tsv", opts.out, g))?);
        write!(w, "row\trec\ttx\tblock\tfrontier")?;
        for s in &src.kmv {
            write!(w, "\t{}:n", s.label)?;
        }
        if src.exact.is_some() {
            write!(w, "\tX_n\tX_n_pos")?;
        }
        writeln!(w)?;
        for (i, &r) in rows.iter().enumerate() {
            let s = &src.sel[r];
            write!(w, "{}\t{}\t{}\t{}\t{}", i, s.rec, s.tx, s.block, s.frontier)?;
            for si in 0..src.kmv.len() {
                write!(w, "\t{}", f(n_row[si][i]))?;
            }
            if let Some(e) = &src.exact {
                write!(w, "\t{}\t{}", e[r].sets.len(), e[r].pos.len())?;
            }
            writeln!(w)?;
        }
        w.flush()?;
        if src.bands.is_some() {
            let tvb: Vec<f64> = outs.iter().map(|o| o.b.unwrap_or(f64::NAN)).collect();
            for (si, s) in src.l1.iter().enumerate() {
                let t: Vec<f64> = outs.iter().map(|o| o.tv[si]).collect();
                checks.push(bands_check(g, &s.label, s.m, &t, &tvb));
            }
        }
        eprintln!("sketch pairs: group {}: {} rows, {} pairs", g, n, pairs.len());
    }
    if src.bands.is_some() {
        write_bands_checks(&format!("{}/bands-check.tsv", opts.out), &checks)?;
    }
    Ok(())
}

/// One row of `bands-check.tsv`.
#[derive(Clone, Debug, PartialEq)]
pub struct BandsCheck {
    pub group: String,
    pub label: String,
    pub m: usize,
    pub pairs: usize,
    pub median_tv1024: f64,
    /// Pairs with `TV-hat >= TV1024 - 3 sqrt(2/m) TV-hat`.
    pub holds: usize,
    /// Pairs with `TV1024 > 0.01`, over which the ratio's median is taken.
    pub over: usize,
    pub median_ratio: f64,
}

/// The lower-bound check of one L1 source over one group's pairs: `t` the
/// L1 estimates, `tvb` the binned exact TVs, pair by pair.  A `NaN`
/// estimate fails the bound, as it does in numpy.
pub fn bands_check(group: &str, label: &str, m: usize, t: &[f64], tvb: &[f64]) -> BandsCheck {
    let slack = 3.0 * (2.0 / m as f64).sqrt();
    let holds = t.iter().zip(tvb).filter(|&(&t, &b)| t >= b - slack * t).count();
    let ratios: Vec<f64> = t.iter().zip(tvb).filter(|x| *x.1 > 0.01).map(|(t, b)| t / b).collect();
    BandsCheck {
        group: group.to_string(),
        label: label.to_string(),
        m,
        pairs: t.len(),
        median_tv1024: metrics::median_mid(tvb),
        holds,
        over: ratios.len(),
        median_ratio: metrics::median_mid(&ratios),
    }
}

fn write_bands_checks(path: &str, rows: &[BandsCheck]) -> io::Result<()> {
    let f = metrics::fmt;
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "group\tlabel\tm\tpairs\tmedian_tv1024\tholds\tshare_holds\tpass\tpairs_tv1024_over_0.01\tmedian_ratio")?;
    for r in rows {
        let share = if r.pairs > 0 { r.holds as f64 / r.pairs as f64 } else { f64::NAN };
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.group,
            r.label,
            r.m,
            r.pairs,
            f(r.median_tv1024),
            r.holds,
            f(share),
            (share >= 0.99) as u8,
            r.over,
            f(r.median_ratio)
        )?;
    }
    w.flush()
}

/// Tab-prefixed columns, no newline.
fn write_cols(w: &mut impl Write, cols: &[String]) -> io::Result<()> {
    for c in cols {
        write!(w, "\t{}", c)?;
    }
    Ok(())
}
