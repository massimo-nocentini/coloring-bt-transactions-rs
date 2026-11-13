//! # `chain` and `check`: the whole chain against what is known exactly
//!
//! Past the first few million records nothing can hold the exact colours side
//! by side with the sketches -- that is the reason for sketching -- but three
//! things about every exact colour are already on disk: its seven Laplace
//! values, its mean and spread, and its mass in each of 1024 bands of 745
//! blocks.  `chain` runs one store per config over the records and, every
//! `--every` records, compares the sketch's estimate with those, folding the
//! errors into per-bucket aggregates (10M records a bucket by default) so the
//! output says how the error grows with depth.
//!
//! `check` computes the same aggregates from a file `emit` wrote, so a stored
//! production file can be validated after the fact.  For that to mean
//! anything the two must agree exactly, so both go through [`Buckets::add`]
//! with exactly the same inputs: the estimate rounded to `f32` as `emit`
//! writes it (never the `f64` one the store had in hand), the `reduced` flag,
//! and `tau` and `raw_mass - 1` as the `f32`s the file carries.  The metric
//! columns of a `chain` run and of a `check` of its `emit` -- same seed, same
//! `--every` and `--bucket`, same references -- are then byte-identical:
//! `buckets.tsv` holds only those, and the store's cost per bucket, which
//! `check` has no store to measure, goes to `bucket_cost.tsv` instead.
//!
//! ## What a bucket reports
//!
//! For each error -- `|dmean|/L`, `|dmean|/ref_sd` (only where the reference
//! spread is at least one block: it comes from `E[b^2] - E[b]^2` in `f64` and
//! cancellation leaves tiny positive spreads behind), `|dsd|/L`, the largest
//! and each of the seven `|dlap_s|`, `|mass_raw - 1|`, and with `--bands-ref`
//! the binned TV, band-edge KS and binned W1 -- the count, mean, RMS and the
//! p50/p90/p99 of a fixed log histogram, over all sampled rows and again over
//! the reduced rows alone.  The second matters because a larger `k` keeps more
//! early colours exact, which lowers an all-rows quantile for a reason that
//! has nothing to do with how well it approximates.  The signed means of
//! `dmean` and each `dlap_s` are reported too (subset `signed`, with their
//! RMS), which is where a bias would show first -- but not with the per-row
//! SEs beside them as its test.  Rows share sketches and inherit their
//! ancestors' sampling error, so `naive_se` (rows as independent draws)
//! understates the spread of a mean many times over, and `block_se` (batch
//! means over blocks of [`metrics::SE_BLOCK`] records) only partly corrects
//! it; both are lower bounds.  Give the same config at several `:seed=`s in
//! one run and `seeds.tsv` reports each bucket's means across the seeds with
//! an honest SE and `z`.
//!
//! The moments file's fourth column is a *binned* participation ratio, not an
//! exact one, and is not read.  Nothing here chooses `k`: these are the
//! tables a person chooses it from.

use crate::algebra::Algebra;
use crate::encode::{round_phat, Meta};
use crate::feed::RefVal;
use crate::metrics::{self, Agg, BlockMeans, BANDS, L, LAPLACE_NAMES};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};

/// The estimate as written and what the record says about itself: the one
/// function both `emit` and `chain` take them from.
pub fn snapshot<A: Algebra>(alg: &A, c: &A::C, p64: &mut Vec<(u32, f64)>, p32: &mut Vec<(u32, f32)>) -> Meta {
    alg.emit(c, p64);
    round_phat(p64, p32);
    let info = alg.info(c);
    if info.reduced {
        Meta { reduced: true, tau: info.tau as f32, mass_m1: (alg.raw_mass(c) - 1.0) as f32 }
    } else {
        Meta::default()
    }
}

const METRICS: [&str; 15] = [
    "abs_dmean_L",
    "dmean_over_sd",
    "abs_dsd_L",
    "dlap_max",
    "abs_dlap_0.5",
    "abs_dlap_1",
    "abs_dlap_2",
    "abs_dlap_4",
    "abs_dlap_8",
    "abs_dlap_16",
    "abs_dlap_32",
    "abs_mass_raw",
    "tv1024",
    "band_ks",
    "band_w1_L",
];
const M: usize = METRICS.len();

#[derive(Clone, Default)]
struct Bucket {
    rows: u64,
    exact: u64,
    all: Vec<Agg>,
    reduced: Vec<Agg>,
    dmean: Agg,
    dlap: Vec<Agg>,
    dmean_bm: BlockMeans,
    dlap_bm: Vec<BlockMeans>,
}

impl Bucket {
    fn new() -> Bucket {
        Bucket {
            all: vec![Agg::default(); M],
            reduced: vec![Agg::default(); M],
            dlap: vec![Agg::default(); 7],
            dlap_bm: vec![BlockMeans::default(); 7],
            ..Default::default()
        }
    }
}

/// A store's cost at the end of a bucket, for `bucket_cost.tsv`.
#[derive(Clone, Debug, Default)]
pub struct BucketCost {
    pub bucket: u64,
    pub busy: f64,
    pub live_entries: u64,
    pub peak_entries: u64,
    pub live_sketches: u64,
    pub map: usize,
    pub reductions: u64,
}

/// The references for one sampled record, any of them possibly absent.
#[derive(Default, Clone, Copy)]
pub struct Refs<'a> {
    pub laplace: Option<&'a [f64; 7]>,
    pub moments: Option<(f64, f64)>,
    pub bands: Option<&'a [(u16, f32)]>,
}

impl<'a> Refs<'a> {
    pub fn from_vals(vals: impl Iterator<Item = Option<&'a RefVal>>) -> Refs<'a> {
        let mut r = Refs::default();
        for v in vals.flatten() {
            match v {
                RefVal::Laplace(l) => r.laplace = Some(l),
                RefVal::Moments { mean, sd } => r.moments = Some((*mean, *sd)),
                RefVal::Bands(b) => r.bands = Some(b),
            }
        }
        r
    }
}

/// Per-bucket aggregates of one config, and its sampled rows.
pub struct Buckets {
    pub label: String,
    bucket: u64,
    rows_every: u64,
    evaluated: u64,
    buckets: BTreeMap<u64, Bucket>,
    rows: Option<BufWriter<File>>,
    p64: Vec<(u32, f64)>,
    est_bins: Vec<f64>,
    ref_bins: Vec<f64>,
}

impl Buckets {
    /// `rows_path`: where to write every `rows_every`-th evaluated record.
    pub fn new(label: String, bucket: u64, rows_every: u64, rows_path: Option<String>) -> io::Result<Buckets> {
        let rows = match rows_path {
            Some(p) => {
                let mut w = BufWriter::new(File::create(&p)?);
                write!(w, "rec\ttx_id\tn\ttau\treduced\tmass_raw\tdmean_L\tdsd_L")?;
                for s in LAPLACE_NAMES {
                    write!(w, "\tdlap_{}", s)?;
                }
                writeln!(w, "\tref_mean_L\tref_sd_L\ttv1024\tband_ks\tband_w1_L")?;
                Some(w)
            }
            None => None,
        };
        Ok(Buckets {
            label,
            bucket: bucket.max(1),
            rows_every: rows_every.max(1),
            evaluated: 0,
            buckets: BTreeMap::new(),
            rows,
            p64: Vec::new(),
            est_bins: vec![0.0; BANDS],
            ref_bins: vec![0.0; BANDS],
        })
    }

    /// Fold one sampled record in.  `p` is the estimate as written.
    pub fn add(&mut self, rec: u64, tx: u64, p: &[(u32, f32)], meta: Meta, refs: Refs) -> io::Result<()> {
        self.p64.clear();
        self.p64.extend(p.iter().map(|&(b, w)| (b, w as f64)));
        let p64 = &self.p64;
        let mut m = [f64::NAN; M];
        let (mut dmean, mut dsd, mut dlap) = (f64::NAN, f64::NAN, [f64::NAN; 7]);
        let (mut ref_mean, mut ref_sd) = (f64::NAN, f64::NAN);
        if let Some((mean, sd)) = refs.moments {
            let (mu, s) = metrics::moments(p64);
            dmean = (mu - mean) / L;
            dsd = (s - sd) / L;
            ref_mean = mean / L;
            ref_sd = sd / L;
            m[0] = dmean.abs();
            if sd >= 1.0 {
                m[1] = (mu - mean).abs() / sd;
            }
            m[2] = dsd.abs();
        }
        if let Some(lap) = refs.laplace {
            let est = metrics::laplace(p64);
            let mut worst = 0.0f64;
            for s in 0..7 {
                dlap[s] = est[s] - lap[s];
                m[4 + s] = dlap[s].abs();
                worst = worst.max(dlap[s].abs());
            }
            m[3] = worst;
        }
        m[11] = (meta.mass_m1 as f64).abs();
        let (mut tv, mut bks, mut bw1) = (f64::NAN, f64::NAN, f64::NAN);
        if let Some(bands) = refs.bands {
            self.est_bins.iter_mut().for_each(|x| *x = 0.0);
            self.ref_bins.iter_mut().for_each(|x| *x = 0.0);
            for &(b, w) in p64.iter() {
                self.est_bins[metrics::band(b)] += w;
            }
            for &(b, w) in bands {
                self.ref_bins[(b as usize).min(BANDS - 1)] += w as f64;
            }
            (tv, bks, bw1) = metrics::band_distances(&self.est_bins, &self.ref_bins);
            m[12] = tv;
            m[13] = bks;
            m[14] = bw1;
        }

        let bucket = self.buckets.entry(rec / self.bucket).or_insert_with(Bucket::new);
        bucket.rows += 1;
        bucket.exact += (!meta.reduced) as u64;
        for i in 0..M {
            bucket.all[i].add(m[i]);
            if meta.reduced {
                bucket.reduced[i].add(m[i]);
            }
        }
        bucket.dmean.add(dmean);
        bucket.dmean_bm.add(rec, dmean);
        for s in 0..7 {
            bucket.dlap[s].add(dlap[s]);
            bucket.dlap_bm[s].add(rec, dlap[s]);
        }

        if self.evaluated % self.rows_every == 0 {
            if let Some(w) = &mut self.rows {
                write!(
                    w,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    rec,
                    tx,
                    p.len(),
                    meta.tau,
                    meta.reduced as u8,
                    metrics::fmt(meta.mass_m1 as f64),
                    metrics::fmt(dmean),
                    metrics::fmt(dsd)
                )?;
                for d in dlap {
                    write!(w, "\t{}", metrics::fmt(d))?;
                }
                writeln!(
                    w,
                    "\t{}\t{}\t{}\t{}\t{}",
                    metrics::fmt(ref_mean),
                    metrics::fmt(ref_sd),
                    metrics::fmt(tv),
                    metrics::fmt(bks),
                    metrics::fmt(bw1)
                )?;
            }
        }
        self.evaluated += 1;
        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if let Some(w) = &mut self.rows {
            w.flush()?;
        }
        Ok(())
    }
}

pub const BUCKETS_HEADER: &str =
    "config\tbucket\trec_from\trec_to\trows\tfrac_exact\tmetric\tsubset\tcount\tmean\trms\tp50\tp90\tp99\tnaive_se\tblock_se\tblocks";

/// One line of `buckets.tsv`: `(metric, subset, aggregate, block means for
/// the signed ones)`.
type Line<'a> = (String, &'static str, &'a Agg, Option<&'a BlockMeans>);

/// A bucket's lines in `buckets.tsv` order.
fn lines(k: &Bucket) -> Vec<Line<'_>> {
    let mut v = Vec::new();
    for (name, (all, red)) in METRICS.iter().zip(k.all.iter().zip(&k.reduced)) {
        v.push((name.to_string(), "all", all, None));
        v.push((name.to_string(), "reduced", red, None));
    }
    v.push(("dmean_L".to_string(), "signed", &k.dmean, Some(&k.dmean_bm)));
    for ((s, a), bm) in LAPLACE_NAMES.iter().zip(&k.dlap).zip(&k.dlap_bm) {
        v.push((format!("dlap_{}", s), "signed", a, Some(bm)));
    }
    v
}

/// Write every config's buckets, config by config, in bucket order.
pub fn write_buckets(path: &str, all: &[Buckets]) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "{}", BUCKETS_HEADER)?;
    for b in all {
        for (&i, k) in &b.buckets {
            let lead = format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                b.label,
                i,
                i * b.bucket,
                (i + 1) * b.bucket - 1,
                k.rows,
                metrics::fmt(k.exact as f64 / k.rows as f64)
            );
            for (name, subset, a, bm) in lines(k) {
                if a.n == 0 {
                    continue;
                }
                write!(w, "{}\t{}\t{}\t{}\t{}\t{}", lead, name, subset, a.n, metrics::fmt(a.mean()), metrics::fmt(a.rms()))?;
                match bm {
                    None => writeln!(
                        w,
                        "\t{}\t{}\t{}\tNA\tNA\tNA",
                        metrics::fmt(a.hist.quantile(0.5)),
                        metrics::fmt(a.hist.quantile(0.9)),
                        metrics::fmt(a.hist.quantile(0.99))
                    )?,
                    Some(bm) => {
                        let (bse, blocks) = bm.se();
                        writeln!(w, "\tNA\tNA\tNA\t{}\t{}\t{}", metrics::fmt(a.se()), metrics::fmt(bse), blocks)?
                    }
                }
            }
        }
    }
    w.flush()
}

/// `seeds.tsv` of a `chain` run: for every family of seed replicates (configs
/// equal but for their seed, at least two of them), each bucket's metric
/// means averaged over the seeds, with their SE and SD across seeds and, for
/// the signed ones, `z` = mean / SE.  The seeds are independent, so this SE is
/// the one a bias question is answered by; the per-row `naive_se` and
/// `block_se` of `buckets.tsv` are lower bounds (see
/// [`metrics::BlockMeans`]).  Removes a stale one when no family has two
/// seeds.
pub fn write_seeds(path: &str, all: &[Buckets]) -> io::Result<()> {
    let mut fams: Vec<(String, Vec<&Buckets>)> = Vec::new();
    for b in all {
        let f = metrics::family(&b.label);
        match fams.iter_mut().find(|x| x.0 == f) {
            Some(x) => x.1.push(b),
            None => fams.push((f, vec![b])),
        }
    }
    if !fams.iter().any(|x| x.1.len() >= 2) {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "family\tbucket\tmetric\tsubset\tseeds\trows\tmean\tseed_se\tseed_sd\tz")?;
    let f = metrics::fmt;
    for (family, members) in fams.iter().filter(|x| x.1.len() >= 2) {
        let ids: std::collections::BTreeSet<u64> = members.iter().flat_map(|b| b.buckets.keys().copied()).collect();
        for i in ids {
            let per: Vec<Vec<Line>> =
                members.iter().filter_map(|b| b.buckets.get(&i)).map(lines).collect();
            for (j, (name, subset, _, bm)) in per[0].iter().enumerate() {
                let means: Vec<f64> = per.iter().map(|l| l[j].2).filter(|a| a.n > 0).map(|a| a.mean()).collect();
                if means.len() < 2 {
                    continue;
                }
                let rows: u64 = per.iter().map(|l| l[j].2.n).sum();
                let (m, se, sd) = metrics::across(&means);
                let z = if bm.is_some() { m / se } else { f64::NAN };
                writeln!(
                    w,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    family,
                    i,
                    name,
                    subset,
                    means.len(),
                    rows,
                    f(m),
                    f(se),
                    f(sd),
                    f(z)
                )?;
            }
        }
    }
    w.flush()
}

pub fn write_bucket_costs(path: &str, all: &[(String, Vec<BucketCost>)]) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "config\tbucket\tbusy_s\tlive_entries\tpeak_entries\tlive_sketches\tmap\treductions")?;
    for (label, costs) in all {
        for c in costs {
            writeln!(
                w,
                "{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{}",
                label, c.bucket, c.busy, c.live_entries, c.peak_entries, c.live_sketches, c.map, c.reductions
            )?;
        }
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_column_names_cover_every_laplace_value() {
        assert_eq!(LAPLACE_NAMES.len(), 7);
        assert_eq!(METRICS.len(), M);
    }

    #[test]
    fn a_perfect_estimate_has_zero_error() {
        let dir = std::env::temp_dir().join(format!("sketch-chain-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut b = Buckets::new("x".into(), 10, 1, None).unwrap();
        let p = vec![(1000u32, 0.5f32), (3000, 0.5)];
        let p64: Vec<(u32, f64)> = p.iter().map(|&(b, w)| (b, w as f64)).collect();
        let lap = metrics::laplace(&p64);
        let (mu, sd) = metrics::moments(&p64);
        let bands = vec![(1u16, 0.5f32), (4, 0.5)];
        let refs = Refs { laplace: Some(&lap), moments: Some((mu, sd)), bands: Some(&bands) };
        b.add(0, 0, &p, Meta::default(), refs).unwrap();
        let k = &b.buckets[&0];
        for i in [0usize, 2, 3, 12, 13, 14] {
            assert_eq!(k.all[i].sum, 0.0, "{}", METRICS[i]);
        }
        let path = dir.join("b.tsv");
        write_buckets(path.to_str().unwrap(), &[b]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.lines().count() > 5);
        std::fs::remove_dir_all(&dir).ok();
    }
}
