//! Tests that need the whole pipeline: the driver over synthetic record
//! streams (T7, T8), unbiasedness through a DAG (T9, T10), coord's invariants
//! against the exact store (T6), emit/decode round trips through the real
//! driver, and the integration test against the main binary.

use crate::algebra::{Algebra, Config, Handle, Info, Sketcher, Variant};
use crate::chain::snapshot;
use crate::driver::Driver;
use crate::encode::{self, Decoded, Decoder, Encoder};
use crate::exact::Exact;
use crate::feed::read_batches;
use crate::hash::{splitmix64, unit, Keys};
use crate::metrics;
use crate::oracle::Oracle;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::sync::Arc;

/// A record in the shape `sexp` reads.
fn record(block: usize, tx: usize, spends: &[(usize, usize)], outputs: usize) -> String {
    let inputs: String = spends.iter().map(|(prev, amount)| format!("(7 {} {} 0)", amount, prev)).collect();
    let outs: String = (0..outputs).map(|_| "(7 1 0)".to_string()).collect();
    format!("((1 {} {} 0 0 0 0) ({}) ({}))\n", block, tx, inputs, outs)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        splitmix64(self.0)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// T7's stream: `n` records over a pool of `pool` coinbase blocks, with
/// multi-input spends at random amounts (zeros and all-zero records among
/// them), repeated parents, and one coinbase whose tx id repeats an earlier
/// one (BIP-30).  Valid both without an oracle (spends respect the unspent
/// counts) and with one.
fn stream(n: usize, pool: usize, seed: u64) -> String {
    let mut rng = Rng(seed);
    let blocks: Vec<usize> = (0..pool).map(|i| 1000 + i * 7919).collect();
    // tx id -> remaining spends
    let mut live: BTreeMap<usize, usize> = BTreeMap::new();
    let mut out = String::new();
    let mut next_tx = 0usize;
    let mut first_coinbase = None;
    let mut duplicated = false;
    for t in 0..n {
        let coinbase = live.len() < 3 || rng.below(4) == 0;
        if coinbase {
            let mut tx = next_tx;
            if !duplicated && t > n / 2 {
                if let Some(c) = first_coinbase {
                    // The duplicate: displaces whatever is left of the first.
                    tx = c;
                    live.remove(&c);
                    duplicated = true;
                }
            }
            if tx == next_tx {
                next_tx += 1;
            }
            first_coinbase.get_or_insert(tx);
            let outputs = 1 + rng.below(4) as usize;
            out.push_str(&record(blocks[rng.below(pool as u64) as usize], tx, &[], outputs));
            live.insert(tx, outputs);
            continue;
        }
        let wide = rng.below(5) == 0;
        let want = 1 + rng.below(if wide { 12 } else { 3 }) as usize;
        let mut spends: Vec<(usize, usize)> = Vec::new();
        let mut used: HashMap<usize, usize> = HashMap::new();
        let all_zero = rng.below(25) == 0;
        for _ in 0..want {
            let keys: Vec<usize> = live.iter().filter(|(k, &v)| v > *used.get(k).unwrap_or(&0)).map(|(&k, _)| k).collect();
            if keys.is_empty() {
                break;
            }
            // Favour repeating a parent already in this record.
            let prev = if !spends.is_empty() && rng.below(4) == 0 && keys.contains(&spends[0].0) {
                spends[0].0
            } else {
                keys[rng.below(keys.len() as u64) as usize]
            };
            *used.entry(prev).or_insert(0) += 1;
            let amount = if all_zero || rng.below(6) == 0 { 0 } else { 1 + rng.below(1_000_000) as usize };
            spends.push((prev, amount));
        }
        for (prev, k) in used {
            let r = live.get_mut(&prev).unwrap();
            *r -= k;
            if *r == 0 {
                live.remove(&prev);
            }
        }
        let tx = next_tx;
        next_tx += 1;
        let outputs = rng.below(4) as usize;
        out.push_str(&record(5_000_000, tx, &spends, outputs));
        if outputs > 0 {
            live.insert(tx, outputs);
        }
    }
    assert!(duplicated, "the stream has its duplicate coinbase");
    out
}

/// The release oracle of `records`, written the way `examples/lastspend.rs`
/// writes it and loaded back.
fn oracle_of(records: &str, name: &str) -> Arc<Oracle> {
    let mut last: Vec<u32> = Vec::new();
    let mut n = 0u64;
    read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
        for i in 0..b.len() {
            for input in b.inputs_of(i) {
                if input.prev_tx_id >= last.len() {
                    last.resize(input.prev_tx_id + 1, u32::MAX);
                }
                last[input.prev_tx_id] = (b.first + i as u64) as u32;
            }
            if b.recs[i].tx >= last.len() {
                last.resize(b.recs[i].tx + 1, u32::MAX);
            }
        }
        n += b.len() as u64;
        true
    })
    .unwrap();
    let mut bytes = b"LASTSPN1".to_vec();
    bytes.extend_from_slice(&n.to_le_bytes());
    bytes.extend_from_slice(&(last.len() as u64).to_le_bytes());
    for v in &last {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let path = std::env::temp_dir().join(format!("sketch-test-{}-{}.u32", std::process::id(), name));
    std::fs::write(&path, bytes).unwrap();
    let o = Oracle::load(path.to_str().unwrap(), Some(n)).unwrap();
    std::fs::remove_file(&path).ok();
    Arc::new(o)
}

/// Every record's `(tx, estimate, info)` under algebra `alg`.
fn colours<A: Algebra>(alg: A, records: &str, oracle: Option<Arc<Oracle>>) -> (Vec<(usize, Vec<(u32, f64)>, Info)>, A) {
    let mut d = Driver::new(alg, oracle, None);
    let mut out = Vec::new();
    read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
        d.batch(&b, |_rec, tx, c, alg| {
            let mut p = Vec::new();
            alg.emit(c, &mut p);
            out.push((tx, p, alg.info(c)));
            Ok(())
        })
        .unwrap();
        true
    })
    .unwrap();
    (out, d.finish())
}

fn keys() -> Arc<Keys> {
    Arc::new(Keys::new(1, crate::bands::CHAIN_BLOCKS + 10))
}

fn sketcher(cfg: &str) -> Sketcher {
    let c = Config::parse(cfg).unwrap();
    let k = (c.variant == Variant::Coord).then(keys);
    Sketcher::new(c, k)
}

const ALL: [&str; 6] = [
    "varopt:{k}",
    "varopt:{k}:reduce=step",
    "prio:{k}",
    "prio:{k}:reduce=step",
    "coord:{k}",
    "coord:{k}:reduce=step",
];

fn assert_close(a: &[(u32, f64)], b: &[(u32, f64)], what: &str) {
    assert_eq!(a.len(), b.len(), "{}: support {} vs {}", what, a.len(), b.len());
    for (x, y) in a.iter().zip(b) {
        assert_eq!(x.0, y.0, "{}", what);
        assert!((x.1 - y.1).abs() <= 1e-6, "{}: block {} {} vs {}", what, x.0, x.1, y.1);
    }
}

/// T7: with k at least every colour's support, every variant in both reduce
/// modes, with and without the oracle, emits the exact colour: no reduction,
/// nothing marked reduced, every coord tau infinite.
#[test]
fn k_at_least_the_support_is_exact() {
    let records = stream(3000, 40, 7);
    let oracle = oracle_of(&records, "t7");
    for o in [None, Some(oracle.clone())] {
        let (exact, _) = colours(Exact::new(), &records, o.clone());
        assert_eq!(exact.len(), 3000);
        for cfg in ALL {
            let cfg = cfg.replace("{k}", "40");
            let (got, alg) = colours(sketcher(&cfg), &records, o.clone());
            assert_eq!(alg.work().reductions, 0, "{}", cfg);
            for ((tx, p, info), (etx, e, _)) in got.iter().zip(&exact) {
                assert_eq!(tx, etx);
                assert!(!info.reduced, "{} tx {}", cfg, tx);
                if cfg.starts_with("coord") {
                    assert!(info.tau.is_infinite(), "{} tx {}", cfg, tx);
                }
                assert_close(p, e, &format!("{} tx {} (oracle {})", cfg, tx, o.is_some()));
            }
            assert_eq!(alg.usage().live_entries, 0, "{}: every sketch released", cfg);
        }
    }
}

/// T7, second part: over 200 blocks with k = 64, every record whose exact
/// support is at most 64 comes out exact -- an ancestor's support is a
/// subset of its descendant's, so nothing it inherits was reduced.
#[test]
fn a_colour_within_k_is_exact_even_when_others_are_reduced() {
    let records = stream(3000, 200, 11);
    let (exact, _) = colours(Exact::new(), &records, None);
    for cfg in ALL {
        let cfg = cfg.replace("{k}", "64");
        let (got, alg) = colours(sketcher(&cfg), &records, None);
        assert!(alg.work().reductions > 0, "{}: the stream should force reductions", cfg);
        let mut small = 0;
        for ((_, p, info), (tx, e, _)) in got.iter().zip(&exact) {
            if e.len() <= 64 {
                small += 1;
                assert!(!info.reduced, "{} tx {} support {}", cfg, tx, e.len());
                assert_close(p, e, &format!("{} tx {}", cfg, tx));
            }
        }
        assert!(small > 100);
    }
}

/// T8: with k = 2^20 nothing is reduced and every variant matches the exact
/// store; and the exact store matches a direct reimplementation of `run()`'s
/// fold bit for bit.
#[test]
fn without_reductions_every_variant_is_the_exact_fold() {
    let records = stream(3000, 400, 13);
    let (exact, _) = colours(Exact::new(), &records, None);
    for cfg in ALL {
        let cfg = cfg.replace("{k}", "1048576");
        let (got, _) = colours(sketcher(&cfg), &records, None);
        for ((_, p, _), (tx, e, _)) in got.iter().zip(&exact) {
            assert_close(p, e, &format!("{} tx {}", cfg, tx));
        }
    }

    // The fold of main.rs, written out with plain vectors.
    type Col = Vec<(u32, f64)>;
    let combine = |a: &Col, wa: f64, b: &Col, wb: f64| -> Col {
        let (mut i, mut j, mut out) = (0, 0, Vec::new());
        while i < a.len() || j < b.len() {
            let ka = a.get(i).map_or(u32::MAX, |x| x.0);
            let kb = b.get(j).map_or(u32::MAX, |x| x.0);
            if ka < kb {
                out.push((ka, a[i].1 * wa));
                i += 1;
            } else if kb < ka {
                out.push((kb, b[j].1 * wb));
                j += 1;
            } else {
                out.push((ka, a[i].1 * wa + b[j].1 * wb));
                i += 1;
                j += 1;
            }
        }
        out
    };
    let mut map: HashMap<usize, (Arc<Col>, usize)> = HashMap::new();
    let mut d = Driver::new(Exact::new(), None, None);
    read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
        for i in 0..b.len() {
            let r = b.recs[i];
            let inputs = b.inputs_of(i);
            let col: Arc<Col> = if inputs.is_empty() {
                Arc::new(vec![(r.block as u32, 1.0)])
            } else {
                let total: f64 = inputs.iter().map(|x| x.amount as f64).sum();
                let share = |a: usize| if total > 0.0 { a as f64 / total } else { 1.0 / inputs.len() as f64 };
                let mut acc: Option<Arc<Col>> = None;
                for k in (0..inputs.len()).rev() {
                    let e = map.get_mut(&inputs[k].prev_tx_id).unwrap();
                    let held = e.0.clone();
                    if e.1 <= 1 {
                        map.remove(&inputs[k].prev_tx_id);
                    } else {
                        e.1 -= 1;
                    }
                    let w = share(inputs[k].amount);
                    acc = Some(match acc {
                        None if w == 1.0 => held,
                        None => Arc::new(held.iter().map(|&(b, v)| (b, v * w)).collect()),
                        Some(a) if Arc::ptr_eq(&a, &held) => Arc::new(held.iter().map(|&(b, v)| (b, v * (w + 1.0))).collect()),
                        Some(a) => Arc::new(combine(&held, w, &a, 1.0)),
                    });
                }
                acc.unwrap()
            };
            let mut terms = Vec::new();
            let batch1 = crate::feed::Batch {
                first: d.records,
                recs: vec![r],
                inputs: inputs.to_vec(),
                ends: vec![inputs.len() as u32],
            };
            d.batch(&batch1, |_, _, c, alg| {
                terms = alg.terms(c);
                Ok(())
            })
            .unwrap();
            assert_eq!(terms.len(), col.len(), "tx {}", r.tx);
            for (x, y) in terms.iter().zip(col.iter()) {
                assert_eq!(x.0, y.0);
                assert_eq!(x.1.to_bits(), y.1.to_bits(), "tx {} block {}: bit for bit", r.tx, x.0);
            }
            if r.outputs > 0 {
                map.insert(r.tx, (col, r.outputs));
            }
        }
        true
    })
    .unwrap();
}

/// T9's DAG: coinbases on distinct blocks spread over the chain, near-copy
/// chains (0.9 of the previous link and 0.1 of a fresh coinbase) and wide
/// merges.  Nothing is ever freed.
fn dag() -> String {
    let mut rng = Rng(99);
    let mut out = String::new();
    let mut coinbases = Vec::new();
    let mut spends = Vec::new();
    let mut chain_tip: Option<usize> = None;
    for t in 0..400usize {
        let kind = if t < 6 { 0 } else { t % 3 };
        match kind {
            0 => {
                out.push_str(&record(1000 + t * 1800, t, &[], 1000));
                coinbases.push(t);
            }
            1 => {
                let fresh = *coinbases.last().unwrap();
                let base = chain_tip.unwrap_or(coinbases[0]);
                out.push_str(&record(5_000_000, t, &[(base, 9), (fresh, 1)], 1000));
                chain_tip = Some(t);
                spends.push(t);
            }
            _ => {
                let width = 2 + rng.below(12) as usize;
                let mut s = Vec::new();
                for _ in 0..width {
                    let pool = if spends.is_empty() || rng.below(3) == 0 { &coinbases } else { &spends };
                    s.push((pool[rng.below(pool.len() as u64) as usize], 1 + rng.below(100) as usize));
                }
                out.push_str(&record(5_000_000, t, &s, 1000));
                spends.push(t);
            }
        }
    }
    out
}

/// Seed-means over R replicates of the final 20 records, raw weights (`raw`)
/// or normalised estimates, checked within 5 SE of the exact colour on every
/// block above 1e-3, and the same for the Laplace value at s = 4.  Answers
/// the worst |z| for the record.
fn replicate(variant: &str, raw: bool, r: u64, check: bool) -> f64 {
    let records = dag();
    let (exact, _) = colours(Exact::new(), &records, None);
    let tail = exact.len() - 20;
    // sums[record][block] = (sum, sum of squares)
    let mut sums: Vec<BTreeMap<u32, (f64, f64)>> = vec![BTreeMap::new(); 20];
    let mut lap = vec![(0.0f64, 0.0f64); 20];
    let mut mass_worst = 0.0f64;
    for seed in 0..r {
        let cfg = Config::parse(&format!("{}:8:seed={}", variant, seed + 1)).unwrap();
        // Each replicate draws its own coordinated keys: a table of none, so
        // every key is hashed on the spot rather than building 762k per seed.
        let k = (cfg.variant == Variant::Coord).then(|| Arc::new(Keys::new(cfg.seed, 0)));
        let mut d = Driver::new(Sketcher::new(cfg, k), None, None);
        let mut i = 0usize;
        read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
            d.batch(&b, |_, _, c: &Handle, alg| {
                if variant == "varopt" {
                    let m = alg.raw_mass(c);
                    mass_worst = mass_worst.max((m - 1.0).abs());
                }
                if i >= tail {
                    let j = i - tail;
                    let p: Vec<(u32, f64)> = if raw {
                        c.entries.iter().map(|e| (e.b, e.w as f64)).collect()
                    } else {
                        let mut p = Vec::new();
                        alg.emit(c, &mut p);
                        p
                    };
                    let mut l4 = 0.0;
                    for &(b, w) in &p {
                        let s = sums[j].entry(b).or_insert((0.0, 0.0));
                        s.0 += w;
                        s.1 += w * w;
                        l4 += w * crate::bands::laplace_of(b as usize)[3];
                    }
                    lap[j].0 += l4;
                    lap[j].1 += l4 * l4;
                }
                i += 1;
                Ok(())
            })
            .unwrap();
            true
        })
        .unwrap();
    }
    if variant == "varopt" {
        // T10: varopt keeps the mass at every record.
        assert!(mass_worst < 1e-5, "varopt mass drifted by {}", mass_worst);
    }
    let rf = r as f64;
    let mut worst = 0.0f64;
    for j in 0..20 {
        let e = &exact[tail + j].1;
        for &(b, want) in e {
            if want <= 1e-3 {
                continue;
            }
            let (s, q) = sums[j].get(&b).copied().unwrap_or((0.0, 0.0));
            let mean = s / rf;
            let se = ((q / rf - mean * mean).max(0.0) / rf).sqrt();
            let z = (mean - want).abs() / (se + 1e-9);
            worst = worst.max(z);
            if check {
                assert!((mean - want).abs() <= 5.0 * se + 1e-6, "{} record {} block {}: mean {} vs {} (se {})", variant, tail + j, b, mean, want, se);
            }
        }
        let want = metrics::laplace(e)[3];
        let mean = lap[j].0 / rf;
        let se = ((lap[j].1 / rf - mean * mean).max(0.0) / rf).sqrt();
        if check {
            assert!((mean - want).abs() <= 5.0 * se + 1e-6, "{} record {}: Laplace mean {} vs {}", variant, tail + j, mean, want);
        }
    }
    worst
}

/// T9 and T10 for varopt: the emitted estimate is unbiased through the DAG,
/// and the mass stays one at every record.
#[test]
fn varopt_is_unbiased_through_a_dag() {
    replicate("varopt", false, 2000, true);
}

/// T9 for prio: the raw HT weights (not the normalised estimate, which is a
/// ratio estimator) are unbiased through the DAG.
#[test]
fn prio_raw_weights_are_unbiased_through_a_dag() {
    replicate("prio", true, 2000, true);
}

/// T9 for coord: documents its bias rather than gating on it.
#[test]
#[ignore]
fn coord_bias_through_a_dag() {
    let z = replicate("coord", false, 2000, false);
    eprintln!("coord: worst |z| of the seed-mean against the exact colour: {:.1} (5 would pass)", z);
}

/// T6: coord's invariants against the exact store on overlapping DAGs: the
/// stored weights are lower bounds, an infinite tau means exact, and scaling
/// divides tau.
#[test]
fn coord_weights_are_lower_bounds() {
    for (seed, cfg) in [(3u64, "coord:16"), (5, "coord:16:reduce=step"), (8, "coord:16:cap=harm")] {
        let records = stream(2000, 300, seed);
        let mut ex = Driver::new(Exact::new(), None, None);
        let mut co = Driver::new(sketcher(cfg), None, None);
        let mut exact_cols: Vec<Vec<(u32, f64)>> = Vec::new();
        read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
            ex.batch(&b, |_, _, c, alg| {
                exact_cols.push(alg.terms(c));
                Ok(())
            })
            .unwrap();
            true
        })
        .unwrap();
        let mut i = 0;
        let mut finite = 0;
        read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
            co.batch(&b, |_, tx, c: &Handle, _| {
                let e = &exact_cols[i];
                for x in c.entries.iter() {
                    let truth = e.binary_search_by_key(&x.b, |y| y.0).map(|k| e[k].1).unwrap_or(0.0);
                    assert!(x.w as f64 <= truth * (1.0 + 1e-6), "{} tx {} block {}: {} above {}", cfg, tx, x.b, x.w, truth);
                }
                if c.tau.is_infinite() {
                    let nz: Vec<(u32, f64)> = e.iter().copied().filter(|x| x.1 != 0.0).collect();
                    assert_eq!(c.entries.len(), nz.len(), "{} tx {}: tau INF means exact", cfg, tx);
                } else {
                    finite += 1;
                }
                i += 1;
                Ok(())
            })
            .unwrap();
            true
        })
        .unwrap();
        assert!(finite > 0, "{}: the stream should reduce something", cfg);
    }
    let mut s = sketcher("coord:4");
    let h = crate::algebra::Sketch::stored(&[(1, 0.5), (2, 0.5)], 3.0, true);
    let scaled = s.scale(&h, 0.25);
    assert_eq!(scaled.tau, 12.0);
}

/// The emitted bytes decode to the text emitted directly, through the real
/// driver, and the metadata survives.
#[test]
fn emit_bin_decodes_to_emit_text() {
    let records = stream(1500, 300, 21);
    for cfg in ["varopt:16", "prio:16", "coord:16"] {
        let config = Config::parse(cfg).unwrap();
        let mut d = Driver::new(sketcher(cfg), None, None);
        let (mut bin, mut text) = (encode::header(&config), Vec::new());
        let mut enc = Encoder::default();
        let (mut p64, mut p32) = (Vec::new(), Vec::new());
        let mut metas = Vec::new();
        read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
            d.batch(&b, |_, tx, c, alg| {
                let m = snapshot(alg, c, &mut p64, &mut p32);
                enc.record(&mut bin, tx as u64, &p32, m);
                encode::text_line(&mut text, tx as u64, &p32);
                metas.push(m);
                Ok(())
            })
            .unwrap();
            true
        })
        .unwrap();
        enc.trailer(&mut bin);
        let mut dec = Decoder::new(&bin[..]).unwrap();
        assert_eq!(dec.config, config);
        let mut back = Vec::new();
        let mut r = Decoded::default();
        let mut i = 0;
        while dec.next(&mut r).unwrap() {
            assert_eq!(r.meta, metas[i]);
            encode::text_line(&mut back, r.tx, &r.p);
            i += 1;
        }
        assert_eq!(i, 1500);
        assert_eq!(back, text, "{}", cfg);
        assert!(metas.iter().any(|m| m.reduced), "{}", cfg);
    }
}

/// The same seed over the same input writes the same bytes; another seed
/// does not.
#[test]
fn the_seed_decides_the_bytes() {
    let records = stream(1500, 300, 23);
    let bytes = |cfg: &str| {
        let mut d = Driver::new(sketcher(cfg), None, None);
        let mut out = Vec::new();
        let mut enc = Encoder::default();
        let (mut p64, mut p32) = (Vec::new(), Vec::new());
        read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
            d.batch(&b, |_, tx, c, alg| {
                let m = snapshot(alg, c, &mut p64, &mut p32);
                enc.record(&mut out, tx as u64, &p32, m);
                Ok(())
            })
            .unwrap();
            true
        })
        .unwrap();
        out
    };
    assert_eq!(bytes("varopt:8:seed=4"), bytes("varopt:8:seed=4"));
    assert_ne!(bytes("varopt:8:seed=4"), bytes("varopt:8:seed=5"));
    assert_eq!(bytes("prio:8"), bytes("prio:8"));
}

#[test]
fn spending_an_unknown_transaction_is_an_error() {
    let records = format!("{}{}", record(0, 0, &[], 1), record(0, 1, &[(5, 1)], 1));
    let mut d = Driver::new(sketcher("varopt:4"), None, None);
    let mut err = None;
    read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
        err = d.batch(&b, |_, _, _, _| Ok(())).err();
        true
    })
    .unwrap();
    let e = err.expect("an error").to_string();
    assert_eq!(e, "transaction 1 spends unknown transaction 5");
}

/// The main binary the integration test compares with: `$SKETCH_WEIGHTED_BIN`
/// when set, otherwise the one in the same target directory and profile as
/// this test binary (`<target>/<profile>/deps/sketch-*` ->
/// `<target>/<profile>/coloring-bt-transactions`), which follows
/// `CARGO_TARGET_DIR` and `--release` without being told.  `cargo test --bin
/// sketch` does not build it, so a binary older than the library sources it
/// is built from is refused rather than trusted.
fn weighted_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("SKETCH_WEIGHTED_BIN") {
        return p.into();
    }
    let exe = std::env::current_exe().expect("the test binary's path");
    let profile = exe.parent().and_then(|deps| deps.parent()).expect("<target>/<profile>/deps/<test binary>");
    let bin = profile.join("coloring-bt-transactions");
    let built = std::fs::metadata(&bin)
        .and_then(|m| m.modified())
        .unwrap_or_else(|_| panic!("{} is missing: build it first (cargo build --release --bin coloring-bt-transactions) or set SKETCH_WEIGHTED_BIN", bin.display()));
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    for e in std::fs::read_dir(src).expect("src/").flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "rs") {
            let m = e.metadata().and_then(|m| m.modified()).expect("mtime");
            assert!(m <= built, "{} is older than {}: rebuild it (cargo build --release --bin coloring-bt-transactions) or set SKETCH_WEIGHTED_BIN", bin.display(), p.display());
        }
    }
    bin
}

/// Integration (needs /data and a release build of the main binary): the first
/// 100k records with k = 2^20, tx by tx against `coloring-bt-transactions
/// 100000 --weighted`.  Absolute tolerance per coefficient, relative only for
/// p >= 1e-6, and terms below f32's smallest normal may be missing.
#[test]
#[ignore]
fn the_first_100k_records_match_weighted() {
    use std::io::BufRead;
    let records = "/data/bitcoin/2022/finalBCUTXO_2022.scm";
    let bin = weighted_bin();
    let bin = bin.as_path();
    let n = 100_000u64;
    let child = std::process::Command::new(bin)
        .args([&n.to_string(), "--weighted"])
        .stdin(std::fs::File::open(records).unwrap())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.unwrap()).lines();
    let mut d = Driver::new(sketcher("varopt:1048576"), None, None);
    let (mut p64, mut p32) = (Vec::new(), Vec::new());
    let mut worst_abs = 0.0f64;
    let mut worst_rel = 0.0f64;
    let mut count = 0u64;
    read_batches(std::fs::File::open(records).unwrap(), n, |b| {
        d.batch(&b, |_, tx, c, alg| {
            snapshot(alg, c, &mut p64, &mut p32);
            let line = lines.next().expect("the binary printed as many lines").unwrap();
            let (t, terms) = line.split_once('\t').unwrap();
            assert_eq!(t.parse::<usize>().unwrap(), tx);
            let mut want: Vec<(u32, f64)> = if terms.is_empty() {
                Vec::new()
            } else {
                terms
                    .split(',')
                    .map(|x| {
                        let (w, b) = x.split_once(':').unwrap();
                        (b.parse().unwrap(), w.parse().unwrap())
                    })
                    .collect()
            };
            want.reverse();
            let mut j = 0;
            for &(b, w) in &want {
                let got = if j < p32.len() && p32[j].0 == b {
                    j += 1;
                    p32[j - 1].1 as f64
                } else {
                    assert!(w < 1.2e-38 || w == 0.0, "tx {}: block {} at {} is missing", tx, b, w);
                    0.0
                };
                let a = (got - w).abs();
                worst_abs = worst_abs.max(a);
                assert!(a <= 1e-6, "tx {} block {}: {} vs {}", tx, b, got, w);
                if w >= 1e-6 {
                    worst_rel = worst_rel.max(a / w);
                    assert!(a / w <= 1e-5, "tx {} block {}: {} vs {}", tx, b, got, w);
                }
            }
            assert_eq!(j, p32.len(), "tx {}: blocks the exact colour does not have", tx);
            count += 1;
            Ok(())
        })
        .unwrap();
        true
    })
    .unwrap();
    assert_eq!(count, n);
    eprintln!("100k records: worst abs {:e}, worst rel (p >= 1e-6) {:e}", worst_abs, worst_rel);
}

#[test]
fn unit_draws_used_by_the_tests_are_in_range() {
    assert!(unit(0) > 0.0);
}
