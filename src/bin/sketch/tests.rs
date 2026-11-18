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

// ------------------------------------------------------------------ KMV, L1 and the modes over them

use crate::count::{self, CountOpts};
use crate::hash::Perm;
use crate::kmv::{KmvSpec, KmvStore, Owned, Support};
use crate::l1::{L1Spec, L1Store};
use crate::pick::{self, PickOpts};
use std::collections::BTreeSet;
use std::rc::Rc;

/// The support recurrence written out with sets: the reference the KMV store
/// is checked against, folded by the same driver.
struct SetAlg {
    pos: bool,
}

impl Algebra for SetAlg {
    type C = Rc<BTreeSet<u32>>;
    fn singleton(&mut self, block: u32) -> Self::C {
        Rc::new([block].into())
    }
    fn record(&mut self, ops: &[(Self::C, f64)], _rec: u64) -> Self::C {
        let mut s = BTreeSet::new();
        for (h, w) in ops {
            if self.pos && *w == 0.0 {
                continue;
            }
            s.extend(h.iter().copied());
        }
        Rc::new(s)
    }
    fn emit(&self, c: &Self::C, out: &mut Vec<(u32, f64)>) {
        out.clear();
        out.extend(c.iter().map(|&b| (b, 1.0)));
    }
    fn raw_mass(&self, _c: &Self::C) -> f64 {
        1.0
    }
    fn info(&self, c: &Self::C) -> Info {
        Info { n: c.len(), tau: 0.0, reduced: false }
    }
    fn work(&self) -> crate::algebra::Work {
        Default::default()
    }
    fn usage(&self) -> crate::algebra::Usage {
        Default::default()
    }
}

/// Every record's colour under `alg`, read out by `get`.
fn fold_all<A: Algebra, T>(alg: A, records: &str, oracle: Option<Arc<Oracle>>, mut get: impl FnMut(&A::C, &A) -> T) -> (Vec<T>, A) {
    let mut d = Driver::new(alg, oracle, None);
    let mut out = Vec::new();
    read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
        d.batch(&b, |_, _, c, alg| {
            out.push(get(c, alg));
            Ok(())
        })
        .unwrap();
        true
    })
    .unwrap();
    (out, d.finish())
}

/// Blocks of `stream()` and `dag()` reach 5,000,000: 23 bits.
const TEST_BITS: u32 = 23;

fn spec(k: usize, seed: u64, support: Support) -> KmvSpec {
    KmvSpec { k, seed, support, bits: TEST_BITS }
}

fn bottom_k(set: &BTreeSet<u32>, perm: &Perm, k: usize) -> Owned {
    let mut r: Vec<u32> = set.iter().map(|&b| perm.rank(b)).collect();
    r.sort_unstable();
    let full = r.len() > k;
    r.truncate(k);
    Owned { r, full }
}

/// T-K3 and T-K5: through the driver, on `stream()` (shared parents,
/// repeated inputs, zero amounts, all-zero records, one-group records, the
/// BIP-30 duplicate) and `dag()`, with and without the oracle, at k in {2,
/// 3, 16, 64}: every record's sketch is `K_k` of its brute-force support,
/// `full` iff the support exceeds k, a whole sketch counts exactly, and the
/// two supports are the exact store's (`sets`: every term, zeros included;
/// `pos`: the terms outside [`Exact::off`], which here are the nonzero ones).
#[test]
fn kmv_is_the_bottom_k_of_the_support_through_the_dag() {
    let perm = Arc::new(Perm::new(7, TEST_BITS));
    for (name, records) in [("stream", stream(2500, 300, 31)), ("dag", dag())] {
        let oracle = oracle_of(&records, &format!("tk3-{}", name));
        for o in [None, Some(oracle)] {
            let (terms, _) = fold_all(Exact::new(), &records, o.clone(), |c, alg| (alg.terms(c), alg.off(c)));
            for support in [Support::Sets, Support::Pos] {
                let pos = support == Support::Pos;
                let (sets, _) = fold_all(SetAlg { pos }, &records, o.clone(), |c, _| (**c).clone());
                for ((t, off), s) in terms.iter().zip(&sets) {
                    let want: BTreeSet<u32> = t.iter().filter(|x| !pos || off.binary_search(&x.0).is_err()).map(|x| x.0).collect();
                    let nonzero: BTreeSet<u32> = t.iter().filter(|x| !pos || x.1 != 0.0).map(|x| x.0).collect();
                    assert_eq!(want, nonzero, "{}: no coefficient underflows here", name);
                    assert_eq!(&want, s, "{} {:?}: the reference support is the exact store's", name, support);
                }
                for k in [2usize, 3, 16, 64] {
                    let alg = KmvStore::new(spec(k, 7, support), perm.clone());
                    let (got, alg) = fold_all(alg, &records, o.clone(), |c, _| (c.r.to_vec(), c.full));
                    let mut full = 0;
                    for (i, ((r, f), s)) in got.iter().zip(&sets).enumerate() {
                        let want = bottom_k(s, &perm, k);
                        assert_eq!((r, *f), (&want.r, want.full), "{} {:?} k {} record {}", name, support, k, i);
                        if !f {
                            assert_eq!(r.len(), s.len());
                        }
                        full += *f as usize;
                    }
                    assert!(k > 16 || full > 0, "{} k {}: something is full", name, k);
                    assert_eq!(alg.usage().live_entries, 0, "every sketch released");
                }
            }
        }
    }
}

/// T-K4: a record's sketch does not depend on the order of its operands, nor
/// on a group being split into repeated operands.
#[test]
fn kmv_ignores_order_and_grouping() {
    let perm = Arc::new(Perm::new(3, 16));
    let mut alg = KmvStore::new(KmvSpec { k: 16, seed: 3, support: Support::Sets, bits: 16 }, perm.clone());
    let mut rng = Rng(5);
    for trial in 0..200u64 {
        let g = 2 + rng.below(40) as usize;
        let parents: Vec<_> = (0..g)
            .map(|_| {
                let n = 1 + rng.below(60) as usize;
                let s: BTreeSet<u32> = (0..n).map(|_| rng.below(5000) as u32).collect();
                let o = bottom_k(&s, &perm, 16);
                crate::kmv::Kmv::new(o.r.into_boxed_slice(), o.full)
            })
            .collect();
        let ops: Vec<(crate::kmv::KHandle, f64)> = parents.iter().map(|p| (p.clone(), 1.0 / g as f64)).collect();
        let a = alg.record(&ops, trial);
        let mut shuffled = ops.clone();
        for i in (1..shuffled.len()).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            shuffled.swap(i, j);
        }
        let b = alg.record(&shuffled, trial);
        let mut repeated = ops.clone();
        for _ in 0..5 {
            let i = rng.below(ops.len() as u64) as usize;
            repeated.push((ops[i].0.clone(), 0.5 / g as f64));
        }
        let c = alg.record(&repeated, trial);
        assert_eq!((&a.r, a.full), (&b.r, b.full));
        assert_eq!((&a.r, a.full), (&c.r, c.full));
    }
}

/// T-L1: the L1 store is the linear map of the exact colour, `v(C_t) = A
/// C_t`, at every record of `dag()` and `stream()`: relative deviation (over
/// `sum_b |A_jb| c_b`) at most 1e-6 in f32 and 1e-12 in f64.  Both the
/// grouped path and the free path (one parent) are on these streams.
#[test]
fn l1_is_the_projection_of_the_exact_colour() {
    for (name, records) in [("stream", stream(1500, 300, 41)), ("dag", dag())] {
        let (terms, _) = fold_all(Exact::new(), &records, None, |c, alg| alg.terms(c));
        for (prec, tol) in [("f32", 1e-6), ("f64", 1e-12)] {
            let spec = L1Spec::parse(&format!("32:seed=4:prec={}", prec)).unwrap();
            let (vs, alg) = fold_all(L1Store::new(spec), &records, None, |c, _| c.to_f64());
            let mut worst = 0.0f64;
            for (t, v) in terms.iter().zip(&vs) {
                for (j, x) in v.iter().enumerate() {
                    let (mut ac, mut den) = (0.0, 0.0);
                    for &(b, c) in t {
                        let a = crate::hash::cauchy(4, b, j as u32);
                        ac += c * a;
                        den += (c * a).abs();
                    }
                    worst = worst.max((x - ac).abs() / den);
                }
            }
            assert!(worst <= tol, "{} {}: worst relative deviation {:e}", name, prec, worst);
            assert_eq!(alg.usage().live_entries, 0, "every vector released");
        }
    }
}

/// A scratch directory of this test process.
fn scratch(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("sketch-test-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_str().unwrap().to_string()
}

fn input(records: &str) -> Box<dyn std::io::Read + Send> {
    Box::new(Cursor::new(records.as_bytes().to_vec()))
}

/// The sets-counts file of `records`, by the brute-force algebra.
fn sets_counts(records: &str, path: &str) {
    let (sizes, _) = fold_all(SetAlg { pos: false }, records, None, |c, _| c.len());
    let text: String = sizes.iter().map(|n| format!("{}\n", n)).collect();
    std::fs::write(path, text).unwrap();
}

fn count_opts(specs: &[KmvSpec], out: &str, refs: Option<String>) -> CountOpts {
    CountOpts {
        kmv: specs.to_vec(),
        out: out.to_string(),
        limit: u64::MAX,
        sets_counts: refs,
        every: Some(7),
        stream: true,
        zstd: false,
        bucket: 1000,
        stats: false,
    }
}

/// `decode` into a file, answering its text.
fn decode_to_string(bytes: Vec<u8>, select: Option<&[u64]>, dir: &str) -> String {
    let path = format!("{}/decoded.txt", dir);
    let out: Box<dyn std::io::Write + Send> = Box::new(std::fs::File::create(&path).unwrap());
    crate::modes::decode(Box::new(Cursor::new(bytes)), out, select).unwrap();
    std::fs::read_to_string(&path).unwrap()
}

/// T-K9 and T-K10 through `count` itself: its stream decodes (dispatched on
/// the magic) to every record's exact size wherever the sketch is whole, the
/// hard counters are 0 against a brute-force sets-counts file, the same seed
/// writes the same bytes and another seed does not, and a selection filters
/// the decoded rows.
#[test]
fn count_streams_decode_and_are_deterministic() {
    let records = stream(3000, 400, 51);
    let dir = scratch("count");
    let refs = format!("{}/sc.txt", dir);
    sets_counts(&records, &refs);
    let (sizes, _) = fold_all(SetAlg { pos: false }, &records, None, |c, _| c.len());
    let specs = [spec(16, 1, Support::Sets), spec(16, 2, Support::Sets), spec(16, 3, Support::Sets), spec(16, 1, Support::Pos)];
    let perms = count::perms_for(&specs);
    let run = |out: &str| {
        count::run(count_opts(&specs, out, Some(refs.clone())), input(&records), None, &perms).unwrap();
    };
    let (a, b) = (format!("{}/a", dir), format!("{}/b", dir));
    run(&a);
    run(&b);
    let bytes = |d: &str, s: &KmvSpec| std::fs::read(format!("{}/counts.{}.bin", d, s.label())).unwrap();
    for s in &specs {
        assert_eq!(bytes(&a, s), bytes(&b, s), "{}: the same seed, the same bytes", s.label());
        assert_eq!(bytes(&a, s).len(), 48 + 20 * 3000 + 24);
    }
    assert_ne!(bytes(&a, &specs[0]), bytes(&a, &specs[1]));
    let check = std::fs::read_to_string(format!("{}/count-check.tsv", a)).unwrap();
    for line in check.lines().skip(1) {
        let c: Vec<&str> = line.split('\t').collect();
        assert_eq!(c[3], "3000", "{}", line);
        assert_eq!(&c[5..9], &["0", "0", "0", "0"], "hard counters: {}", line);
    }
    assert!(std::path::Path::new(&format!("{}/count-seeds.tsv", a)).exists());
    let text = decode_to_string(bytes(&a, &specs[0]), None, &a);
    let mut lines = text.lines();
    // A3: the first line names the stream's spec and support.
    assert_eq!(lines.next().unwrap(), format!("# kmv-k16-s1-sets-b23 k=16 seed=1 support=sets bits=23 M={}", 1u64 << 23));
    assert_eq!(lines.next().unwrap(), "rec\ttx\tblock\tuniv\tfull\tm\tn_hat\tse\tn_hat_M\tse_M");
    let pos = decode_to_string(bytes(&a, &specs[3]), None, &a);
    assert_eq!(pos.lines().next().unwrap(), format!("# kmv-k16-s1-pos-b23 k=16 seed=1 support=pos bits=23 M={}", 1u64 << 23));
    let mut whole = 0;
    for (i, l) in lines.enumerate() {
        let c: Vec<&str> = l.split('\t').collect();
        assert_eq!(c[0], i.to_string());
        if c[4] == "0" {
            whole += 1;
            assert_eq!(c[6], sizes[i].to_string(), "record {}: a whole sketch counts exactly", i);
        } else {
            assert!(sizes[i] > 16);
        }
    }
    assert!(whole > 100 && whole < 3000);
    let text = decode_to_string(bytes(&a, &specs[0]), Some(&[3, 17, 2999]), &a);
    let recs: Vec<&str> = text.lines().skip(2).map(|l| l.split('\t').next().unwrap()).collect();
    assert_eq!(recs, vec!["3", "17", "2999"]);
}

/// numpy's `percentile(v, 100 q)` by a full sort: the reference `summary`'s
/// selection is checked against.
fn q7(mut v: Vec<f64>, q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let h = (v.len() - 1) as f64 * q;
    let lo = h.floor() as usize;
    if lo + 1 >= v.len() {
        return v[v.len() - 1];
    }
    let (a, b, t) = (v[lo], v[lo + 1], h - lo as f64);
    if t >= 0.5 {
        b - (b - a) * (1.0 - t)
    } else {
        a + (b - a) * t
    }
}

/// A `.tsv` with a header, as one map per line.
fn tsv(path: &str) -> Vec<HashMap<String, String>> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    let head: Vec<String> = lines.next().unwrap().split('\t').map(String::from).collect();
    lines.map(|l| head.iter().cloned().zip(l.split('\t').map(String::from)).collect()).collect()
}

/// `summary` over `count`'s own streams.  Every column is what numpy-style
/// quantiles of the decoded rows give (`decode`'s text, read back), on every
/// scope; the reference columns are the brute-force sizes; `--every` is
/// `count-rows`'s sample exactly; a seed pair is the `mawk` forms over the
/// records full in both; a `.zst` input summarises the same; and what is not
/// the streams of one run is refused.
#[test]
fn summary_reads_count_streams_back() {
    use crate::kmv::{CntDecoder, CNT_MAGIC};
    use crate::summary::{self, Kept, Range, SummaryOpts};
    let records = stream(3000, 400, 51);
    let dir = scratch("summary");
    let refs = format!("{}/sc.txt", dir);
    sets_counts(&records, &refs);
    let (sizes, _) = fold_all(SetAlg { pos: false }, &records, None, |c, _| c.len() as f64);
    let specs = [spec(16, 1, Support::Sets), spec(16, 2, Support::Sets), spec(16, 3, Support::Sets), spec(16, 1, Support::Pos)];
    let perms = count::perms_for(&specs);
    let a = format!("{}/a", dir);
    count::run(count_opts(&specs, &a, Some(refs.clone())), input(&records), None, &perms).unwrap();
    let path = |s: &KmvSpec| format!("{}/counts.{}.bin", a, s.label());
    let opts = |inputs: Vec<String>, out: &str, every: u64| SummaryOpts {
        inputs,
        out: out.to_string(),
        limit: u64::MAX,
        every,
        bucket: 1000,
        ranges: vec![Range { first: 500, end: 2100 }, Range { first: 2500, end: u64::MAX }],
        sets_counts: Some(refs.clone()),
    };
    let s = format!("{}/s", dir);
    summary::run(opts(specs.iter().map(path).collect(), &s, 1)).unwrap();
    let lines = tsv(&format!("{}/summary.tsv", s));
    assert_eq!(lines.len(), specs.len() * (1 + 2 + 3), "all, two ranges, three buckets per stream");

    // Every record recomputed from what `Kept` holds is the decoder's row.
    let bytes = std::fs::read(path(&specs[0])).unwrap();
    let mut dec = CntDecoder::new(&bytes[8..], *CNT_MAGIC, None).unwrap();
    while let Some(r) = dec.next().unwrap() {
        assert_eq!(Kept::of(&r).counts(16, dec.m), [r.n_hat, r.se, r.n_hat_m, r.se_m], "record {}", r.rec);
    }

    // rec tx block univ full m n_hat se n_hat_M se_M, per stream.
    let rows = |sp: &KmvSpec| -> Vec<Vec<f64>> {
        decode_to_string(std::fs::read(path(sp)).unwrap(), None, &dir)
            .lines()
            .skip(2)
            .map(|l| l.split('\t').map(|c| c.parse().unwrap()).collect())
            .collect()
    };
    let f = metrics::fmt;
    for sp in &specs {
        let d = rows(sp);
        for l in lines.iter().filter(|l| l["label"] == sp.label()) {
            let (lo, hi): (usize, usize) = (l["first"].parse().unwrap(), l["end"].parse().unwrap());
            let r = &d[lo..hi];
            let full: Vec<&Vec<f64>> = r.iter().filter(|x| x[4] == 1.0).collect();
            let col = |c: usize| r.iter().map(|x| x[c]).collect::<Vec<f64>>();
            let ctx = format!("{} {} {}..{}", sp.label(), l["scope"], lo, hi);
            assert_eq!(l["records"], (hi - lo).to_string(), "{}", ctx);
            assert_eq!(l["full"], full.len().to_string(), "{}", ctx);
            assert_eq!(l["over_univ"], "0", "{}", ctx);
            assert_eq!(l["beta_last"], (r[r.len() - 1][3] as u64 - 1).to_string(), "{}", ctx);
            for (name, q) in [("n_p10", 0.1), ("n_p50", 0.5), ("n_p90", 0.9), ("n_p99", 0.99)] {
                assert_eq!(l[name], f(q7(col(6), q)), "{} {}", ctx, name);
            }
            assert_eq!(l["univ_p50"], f(q7(col(3), 0.5)), "{}", ctx);
            assert_eq!(l["fill_p50"], f(q7(r.iter().map(|x| x[6] / x[3]).collect(), 0.5)), "{}", ctx);
            for (name, q) in [("rse_p10", 0.1), ("rse_p50", 0.5), ("rse_p90", 0.9)] {
                assert_eq!(l[name], f(q7(full.iter().map(|x| x[7] / x[6]).collect(), q)), "{} {}", ctx, name);
            }
            assert_eq!(l["rse_M_p50"], f(q7(full.iter().map(|x| x[9] / x[8]).collect(), 0.5)), "{}", ctx);
            assert_eq!(l["ratio_M_p50"], f(q7(full.iter().map(|x| x[8] / x[6]).collect(), 0.5)), "{}", ctx);
            let whole: Vec<f64> = r.iter().filter(|x| x[4] == 0.0).map(|x| x[6]).collect();
            assert_eq!(l["whole_max"], f(q7(whole.clone(), 1.0)), "{}", ctx);
            assert_eq!(l["whole_p90"], f(q7(whole, 0.9)), "{}", ctx);
            assert_eq!(l["ref_records"], (hi - lo).to_string(), "{}", ctx);
            assert_eq!(l["ref_p50"], f(q7(sizes[lo..hi].to_vec(), 0.5)), "{}", ctx);
            assert_eq!(l["ref_bad"], "0", "{}", ctx);
        }
    }

    // Seeds against each other: pairs within the sets family only.
    let disp = tsv(&format!("{}/dispersion.tsv", s));
    let names: BTreeSet<&str> = disp.iter().map(|l| l["pair"].as_str()).collect();
    assert_eq!(names, BTreeSet::from(["s1-s2", "s1-s3", "s2-s3"]));
    let (d1, d2) = (rows(&specs[0]), rows(&specs[1]));
    for b in 0..3 {
        let (mut c, mut sxx, mut th) = (0.0, 0.0, 0.0);
        for (x, y) in d1[b * 1000..(b + 1) * 1000].iter().zip(&d2[b * 1000..(b + 1) * 1000]) {
            if x[4] == 1.0 && y[4] == 1.0 {
                let e = (x[6] - y[6]) / ((x[6] + y[6]) / 2.0);
                c += 1.0;
                sxx += e * e;
                th += ((x[7] / x[6]).powi(2) + (y[7] / y[6]).powi(2)) / 2.0;
            }
        }
        let l = disp.iter().find(|l| l["pair"] == "s1-s2" && l["bucket"] == b.to_string()).unwrap();
        assert_eq!(l["records"], c.to_string());
        let want = (sxx / c / 2.0).sqrt() / (th / c).sqrt();
        let got: f64 = l["rms_ratio"].parse().unwrap();
        assert!((got - want).abs() < 1e-12, "bucket {}: {} against {}", b, got, want);
    }
    let all = disp.iter().find(|l| l["pair"] == "s1-s2" && l["bucket"] == "all").unwrap();
    assert_ne!(all["min_rms_ratio"], "NA");

    // --every 7 is count's --every 7: the rows of count-rows.
    let e = format!("{}/e", dir);
    summary::run(opts(vec![path(&specs[0])], &e, 7)).unwrap();
    let sample: Vec<f64> = tsv(&format!("{}/count-rows.{}.tsv", a, specs[0].label())).iter().map(|l| l["n_hat"].parse().unwrap()).collect();
    let l = &tsv(&format!("{}/summary.tsv", e))[0];
    assert_eq!(l["records"], sample.len().to_string());
    assert_eq!(l["n_p50"], f(q7(sample.clone(), 0.5)));
    assert_eq!(l["n_p90"], f(q7(sample, 0.9)));
    assert!(!std::path::Path::new(&format!("{}/dispersion.tsv", e)).exists(), "one stream, no pairs");

    // A .zst input reads the same.
    let plain = path(&specs[0]);
    let st = std::process::Command::new("zstd").args(["-q", "-f", &plain, "-o", &format!("{}/s1.bin.zst", dir)]).status().unwrap();
    assert!(st.success());
    let (p, z) = (format!("{}/p", dir), format!("{}/z", dir));
    summary::run(opts(vec![plain.clone()], &p, 1)).unwrap();
    summary::run(opts(vec![format!("{}/s1.bin.zst", dir)], &z, 1)).unwrap();
    assert_eq!(std::fs::read(format!("{}/summary.tsv", p)).unwrap(), std::fs::read(format!("{}/summary.tsv", z)).unwrap());

    // Refused: a spec twice, a stream of another run, a shorter one, a
    // file that is not a count stream, and a reference of other records.
    let err = |inputs: Vec<String>, sc: Option<String>| {
        let mut o = opts(inputs, &format!("{}/x", dir), 1);
        o.sets_counts = sc;
        summary::run(o).unwrap_err().to_string()
    };
    assert!(err(vec![plain.clone(), plain.clone()], None).contains("given twice"));
    // Another run: seed 2's stream with record 5's tx changed, and the
    // trailer's checksum patched to match, so only the tx disagrees.
    let mut b = std::fs::read(path(&specs[1])).unwrap();
    let o = 48 + 20 * 5;
    let word = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    let (old, sum_at) = (word(&b, o), b.len() - 8);
    let (m, rk) = (u32::from_le_bytes(b[o + 12..o + 16].try_into().unwrap()), u32::from_le_bytes(b[o + 16..o + 20].try_into().unwrap()));
    let check = |tx: u64| tx ^ m as u64 ^ ((rk as u64) << 32);
    let sum = word(&b, sum_at).wrapping_sub(check(old)).wrapping_add(check(old + 1_000_000));
    b[o..o + 8].copy_from_slice(&(old + 1_000_000).to_le_bytes());
    b[sum_at..].copy_from_slice(&sum.to_le_bytes());
    let other = format!("{}/other.bin", dir);
    std::fs::write(&other, &b).unwrap();
    let e = err(vec![plain.clone(), other], None);
    assert!(e.contains("record 5: tx") && e.contains("one count run"), "{}", e);
    let other_records = stream(3000, 400, 52);
    let short = format!("{}/short", dir);
    let mut o = count_opts(&specs[1..2], &short, None);
    o.limit = 2000;
    count::run(o, input(&records), None, &perms).unwrap();
    let e = err(vec![plain.clone(), format!("{}/counts.{}.bin", short, specs[1].label())], None);
    assert!(e.contains("different numbers of records"), "{}", e);
    assert!(err(vec![refs.clone()], None).contains("not a count stream"));
    let wrong = format!("{}/wrong.txt", dir);
    sets_counts(&other_records, &wrong);
    assert!(err(vec![plain], Some(wrong)).contains("disagree with --sets-counts"));
}

/// A5: what the old modes write does not change.  The SKETCH01 header is
/// pinned byte for byte, `decode` without a selection is the text `emit`
/// would write, and `decode --select` prefixes exactly the selected lines.
#[test]
fn sketch01_and_decode_are_unchanged() {
    let cfg = Config::parse("varopt:16:seed=3").unwrap();
    assert_eq!(
        encode::header(&cfg),
        [
            b'S', b'K', b'E', b'T', b'C', b'H', b'0', b'1', 1, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0
        ]
    );
    let records = stream(1500, 300, 61);
    let mut d = Driver::new(sketcher("varopt:16:seed=3"), None, None);
    let (mut bin, mut text) = (encode::header(&cfg), Vec::new());
    let mut enc = Encoder::default();
    let (mut p64, mut p32) = (Vec::new(), Vec::new());
    let mut lines = Vec::new();
    read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
        d.batch(&b, |_, tx, c, alg| {
            let m = snapshot(alg, c, &mut p64, &mut p32);
            enc.record(&mut bin, tx as u64, &p32, m);
            let mut one = Vec::new();
            encode::text_line(&mut one, tx as u64, &p32);
            text.extend_from_slice(&one);
            lines.push((m, String::from_utf8(one).unwrap()));
            Ok(())
        })
        .unwrap();
        true
    })
    .unwrap();
    enc.trailer(&mut bin);
    let dir = scratch("decode");
    assert_eq!(decode_to_string(bin.clone(), None, &dir).as_bytes(), &text[..]);
    let sel = [0u64, 5, 1499];
    let got = decode_to_string(bin, Some(&sel), &dir);
    let want: String = sel
        .iter()
        .map(|&r| {
            let (m, l) = &lines[r as usize];
            format!("{}\t{}\t{}\t{}", r, m.reduced as u8, m.tau, l)
        })
        .collect();
    assert_eq!(got, want);
}

/// T-P2 and T-P1: `pick` dumps each selected record once, in order, and the
/// varopt subset decodes to what `emit` writes at those indices; `pairs` on
/// the dump equals the pure estimators, its matrices are symmetric with the
/// diagonal at 1, its exact columns are brute force, and its output is the
/// same bytes at 1 and 8 threads.
#[test]
fn pick_and_pairs_agree_with_the_pure_functions() {
    let records = stream(2000, 300, 71);
    let dir = scratch("pick");
    let sel_path = format!("{}/sel.txt", dir);
    let chosen: Vec<u64> = vec![1999, 3, 40, 41, 500, 777, 1200, 1500, 1501, 1800];
    let text: String = chosen.iter().enumerate().map(|(i, r)| format!("{}\t{}\n", r, if i % 3 == 0 { "odd" } else { "all" })).collect();
    std::fs::write(&sel_path, format!("# test selection\n{}", text)).unwrap();
    let kspecs = [spec(8, 1, Support::Sets), spec(8, 1, Support::Pos)];
    let lspecs = [L1Spec::parse("64").unwrap(), L1Spec::parse("64:prec=f64").unwrap()];
    let perms = count::perms_for(&kspecs);
    let out = format!("{}/pick", dir);
    let opts = |out: &str, sel: &str| PickOpts {
        select: sel.to_string(),
        out: out.to_string(),
        limit: u64::MAX,
        configs: vec![Config::parse("varopt:16").unwrap()],
        kmv: kspecs.to_vec(),
        l1: lspecs.to_vec(),
        exact: true,
        stats: false,
    };
    pick::run(opts(&out, &sel_path), input(&records), None, &HashMap::new(), &perms).unwrap();
    // Out of range: an error.
    let bad = format!("{}/bad.txt", dir);
    std::fs::write(&bad, "5\n2000\n").unwrap();
    let e = pick::run(opts(&format!("{}/bad", dir), &bad), input(&records), None, &HashMap::new(), &perms).unwrap_err();
    assert!(e.to_string().contains("names record 2000"), "{}", e);

    // T-K10 for pick: the same seeds write the same bytes, another does not.
    let again = format!("{}/again", dir);
    pick::run(opts(&again, &sel_path), input(&records), None, &HashMap::new(), &perms).unwrap();
    for f in ["kmv.kmv-k8-s1-sets-b23.bin", "l1.l1-m64-s1-f32.bin", "varopt.varopt-k16-s1-rec.bin", "exact.bin", "sel.tsv"] {
        assert_eq!(std::fs::read(format!("{}/{}", out, f)).unwrap(), std::fs::read(format!("{}/{}", again, f)).unwrap(), "{}", f);
    }
    let other = [spec(8, 2, Support::Sets)];
    let o2 = format!("{}/other", dir);
    let mut po = opts(&o2, &sel_path);
    po.kmv = other.to_vec();
    pick::run(po, input(&records), None, &HashMap::new(), &count::perms_for(&other)).unwrap();
    assert_ne!(
        std::fs::read(format!("{}/kmv.kmv-k8-s1-sets-b23.bin", out)).unwrap()[32..],
        std::fs::read(format!("{}/kmv.kmv-k8-s2-sets-b23.bin", o2)).unwrap()[32..]
    );

    let mut sorted = chosen.clone();
    sorted.sort();
    let sel = pick::read_sel_tsv(&format!("{}/sel.tsv", out)).unwrap();
    assert_eq!(sel.iter().map(|r| r.rec).collect::<Vec<_>>(), sorted);
    // The varopt dump decodes to emit's records at those indices.
    let mut d = Driver::new(sketcher("varopt:16"), None, None);
    let (mut p64, mut p32) = (Vec::new(), Vec::new());
    let mut emitted = Vec::new();
    read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
        d.batch(&b, |_, tx, c, alg| {
            let m = snapshot(alg, c, &mut p64, &mut p32);
            emitted.push((tx as u64, m, p32.clone()));
            Ok(())
        })
        .unwrap();
        true
    })
    .unwrap();
    let mut dec = Decoder::new(std::fs::File::open(format!("{}/varopt.varopt-k16-s1-rec.bin", out)).unwrap()).unwrap();
    let mut r = Decoded::default();
    let mut i = 0;
    while dec.next(&mut r).unwrap() {
        let (tx, m, p) = &emitted[sorted[i] as usize];
        assert_eq!((r.tx, r.meta, &r.p), (*tx, *m, p));
        i += 1;
    }
    assert_eq!(i, sorted.len());

    // The supports of the selected records, brute force.
    let (all_sets, _) = fold_all(SetAlg { pos: false }, &records, None, |c, _| (**c).clone());
    let (all_pos, _) = fold_all(SetAlg { pos: true }, &records, None, |c, _| (**c).clone());
    let (one, eight) = (format!("{}/pairs1", dir), format!("{}/pairs8", dir));
    for (o, t) in [(&one, 1), (&eight, 8)] {
        crate::pairs::run(crate::pairs::PairsOpts { input: out.clone(), out: o.clone(), groups: vec![], varopt_text: None, threads: t })
            .unwrap();
    }
    let mut names: Vec<String> = std::fs::read_dir(&one).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert!(names.contains(&"pairs.all.tsv".to_string()) && names.contains(&"pairs.odd.tsv".to_string()));
    for n in &names {
        assert_eq!(std::fs::read(format!("{}/{}", one, n)).unwrap(), std::fs::read(format!("{}/{}", eight, n)).unwrap(), "{}", n);
    }
    let tsv = std::fs::read_to_string(format!("{}/pairs.all.tsv", one)).unwrap();
    let mut lines = tsv.lines();
    let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
    let col = |name: &str| header.iter().position(|h| *h == name).unwrap_or_else(|| panic!("no column {}", name));
    let rows_all: Vec<&pick::SelRow> = sel.iter().filter(|r| r.group == "all").collect();
    let perm = &perms[&(1, TEST_BITS)];
    // A1's restricted counts, brute force: `R''` by a scan of the universe
    // `[0, N)` (no Fenwick tree, no offline sweep), `N = frontier + 1` for a
    // row and `max(frontier_i, frontier_j) + 1` for a pair.
    let below = |n_univ: u64, rk: u32| (0..n_univ).filter(|&b| perm.rank(b as u32) < rk).count() as u64;
    let count_bf = |sk: &Owned, n_univ: u64| match sk.view().rk() {
        None => sk.r.len() as f64,
        Some(rk) => 7.0 * n_univ as f64 / below(n_univ, rk) as f64,
    };
    let mut pairs = 0;
    let mut full_unions = 0;
    for l in lines {
        let c: Vec<&str> = l.split('\t').collect();
        let (a, b): (usize, usize) = (c[0].parse().unwrap(), c[1].parse().unwrap());
        let (ra, rb) = (rows_all[a].rec as usize, rows_all[b].rec as usize);
        let (na_univ, nb_univ) = (rows_all[a].frontier as u64 + 1, rows_all[b].frontier as u64 + 1);
        let nu_univ = na_univ.max(nb_univ);
        for (label, sets) in [("kmv-k8-s1-sets-b23", &all_sets), ("kmv-k8-s1-pos-b23", &all_pos)] {
            let (ka, kb) = (bottom_k(&sets[ra], perm, 8), bottom_k(&sets[rb], perm, 8));
            let e = crate::kmv::pair(ka.view(), kb.view(), 8, perm.universe());
            assert_eq!(c[col(&format!("{}:J", label))], metrics::fmt(e.j), "{} pair {} {}", label, a, b);
            assert_eq!(c[col(&format!("{}:cont_ij", label))], metrics::fmt(e.cont_ab));
            assert_eq!(c[col(&format!("{}:exact", label))], (e.exact as u8).to_string());
            // The counts: per row over its own universe, per pair over the
            // larger of the two, and the M column as before.
            let (na, nb) = (count_bf(&ka, na_univ), count_bf(&kb, nb_univ));
            assert_eq!(c[col(&format!("{}:n_i", label))], metrics::fmt(na), "{} pair {} {}: n_i", label, a, b);
            assert_eq!(c[col(&format!("{}:n_j", label))], metrics::fmt(nb), "{} pair {} {}: n_j", label, a, b);
            let raw = crate::kmv::pair_raw(ka.view(), kb.view(), 8);
            let nu = match raw.union_rk {
                None => {
                    let union = sets[ra].union(&sets[rb]).count();
                    if e.exact {
                        assert_eq!(raw.union as usize, union);
                    }
                    raw.union as f64
                }
                Some(rk) => {
                    full_unions += 1;
                    7.0 * nu_univ as f64 / below(nu_univ, rk) as f64
                }
            };
            assert_eq!(c[col(&format!("{}:n_union", label))], metrics::fmt(nu), "{} pair {} {}: n_union", label, a, b);
            assert!(nu <= nu_univ as f64 || e.exact, "the restricted union count is at most its universe");
            assert_eq!(c[col(&format!("{}:n_union_M", label))], metrics::fmt(e.n_union), "{} pair {} {}: n_union_M", label, a, b);
            assert_eq!(c[col(&format!("{}:inter", label))], metrics::fmt(e.j * nu));
            assert_eq!(c[col(&format!("{}:cont_ij_fb", label))], metrics::fmt(e.j * nu / na));
            assert_eq!(c[col(&format!("{}:cont_ji_fb", label))], metrics::fmt(e.j * nu / nb));
            if e.exact {
                let inter = sets[ra].intersection(&sets[rb]).count() as f64;
                let union = sets[ra].union(&sets[rb]).count() as f64;
                assert_eq!(e.j, inter / union);
            }
        }
        let (sa, sb) = (&all_sets[ra], &all_sets[rb]);
        let xj = sa.intersection(sb).count() as f64 / sa.union(sb).count() as f64;
        assert_eq!(c[col("X_J")], metrics::fmt(xj));
        assert_eq!(c[col("X_n_i")], sa.len().to_string());
        let (pa, pb) = (&all_pos[ra], &all_pos[rb]);
        assert_eq!(c[col("X_J_pos")], metrics::fmt(pa.intersection(pb).count() as f64 / pa.union(pb).count() as f64));
        // The weighted Jaccard two ways: sum min / sum max, and (1-TV)/(1+TV).
        let (tv, jw): (f64, f64) = (c[col("X_tv")].parse().unwrap(), c[col("X_jw")].parse().unwrap());
        assert!((jw - (1.0 - tv) / (1.0 + tv)).abs() < 1e-9);
        let (t32, t64): (f64, f64) = (c[col("l1-m64-s1-f32:tv")].parse().unwrap(), c[col("l1-m64-s1-f64:tv")].parse().unwrap());
        assert!((t32 - t64).abs() <= 1e-5 * t64.max(1e-3), "f32 {} f64 {}", t32, t64);
        pairs += 1;
    }
    let n = rows_all.len();
    assert_eq!(pairs, n * (n - 1) / 2);
    assert!(full_unions > 0, "some unions are full, so the restricted union count is exercised");
    // The row counts as `pairs` writes them for plots: `n.<g>.<label>.f32`
    // and `rows.<g>.tsv`, the same restricted counts.
    let rows_tsv = std::fs::read_to_string(format!("{}/rows.all.tsv", one)).unwrap();
    let mut rl = rows_tsv.lines();
    let rh: Vec<&str> = rl.next().unwrap().split('\t').collect();
    let rows_tsv: Vec<Vec<&str>> = rl.map(|l| l.split('\t').collect()).collect();
    assert_eq!(rows_tsv.len(), n);
    let mut full_rows = 0;
    for (label, sets) in [("kmv-k8-s1-sets-b23", &all_sets), ("kmv-k8-s1-pos-b23", &all_pos)] {
        let v = std::fs::read(format!("{}/n.all.{}.f32", one, label)).unwrap();
        let v: Vec<f32> = v.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(v.len(), n);
        let rc = rh.iter().position(|h| *h == format!("{}:n", label)).unwrap();
        for (i, r) in rows_all.iter().enumerate() {
            let sk = bottom_k(&sets[r.rec as usize], perm, 8);
            full_rows += sk.full as usize;
            let want = count_bf(&sk, r.frontier as u64 + 1);
            assert_eq!(v[i], want as f32, "{} row {}", label, i);
            assert_eq!(rows_tsv[i][rc], metrics::fmt(want), "{} row {}", label, i);
            assert_eq!(rows_tsv[i][4], r.frontier.to_string());
        }
    }
    assert!(full_rows > 0, "some rows are full, so the restricted row count is exercised");
    let m = std::fs::read(format!("{}/m.all.kmv-k8-s1-sets-b23.J.f32", one)).unwrap();
    let v: Vec<f32> = m.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(v.len(), n * n);
    for a in 0..n {
        assert_eq!(v[a * n + a], 1.0);
        for b in 0..n {
            assert_eq!(v[a * n + b], v[b * n + a]);
        }
    }
}

/// T-E1: `eval --kmv` over `stream()` finds every sketch equal to the
/// bottom-k of the exact support (`set_mismatch = 0`, both supports) and the
/// exact support equal to a sets-counts file written by the brute-force
/// algebra; and without the new flags `eval` writes the files and columns it
/// always wrote (A5).
#[test]
fn eval_checks_kmv_against_the_exact_store() {
    let records = stream(3000, 400, 81);
    let dir = scratch("eval");
    let refs = format!("{}/sc.txt", dir);
    sets_counts(&records, &refs);
    let kspecs = vec![spec(16, 1, Support::Sets), spec(16, 2, Support::Sets), spec(16, 1, Support::Pos)];
    let opts = |out: &str, configs: Vec<Config>, kmv: Vec<KmvSpec>, l1: Vec<L1Spec>, sc: Option<String>| crate::eval::EvalOpts {
        configs,
        out: out.to_string(),
        limit: u64::MAX,
        every: 7,
        metric_threads: 3,
        check_ref: None,
        stats: false,
        perms: count::perms_for(&kmv),
        kmv,
        l1,
        l1_every: 101,
        sets_counts: sc,
    };
    let out = format!("{}/side", dir);
    crate::eval::run(
        opts(&out, vec![Config::parse("varopt:16").unwrap()], kspecs.clone(), vec![L1Spec::parse("32").unwrap()], Some(refs.clone())),
        input(&records),
        None,
        HashMap::new(),
    )
    .unwrap();
    let summary = std::fs::read_to_string(format!("{}/kmv-summary.tsv", out)).unwrap();
    let mut seen = 0;
    for l in summary.lines() {
        let c: Vec<&str> = l.split('\t').collect();
        if c[3] == "set_mismatch" || c[3] == "support_ref_mismatch" {
            assert_eq!(c[5], "0", "{}", l);
            assert!(c[4].parse::<u64>().unwrap() > 400, "{}", l);
            seen += 1;
        }
    }
    assert_eq!(seen, 4);
    let l1 = std::fs::read_to_string(format!("{}/l1-summary.tsv", out)).unwrap();
    let worst: f64 = l1.lines().nth(1).unwrap().split('\t').nth(2).unwrap().parse().unwrap();
    assert!(worst < 1e-6, "{}", l1);
    // A5: the old outputs, alone and unchanged in shape.
    let old = format!("{}/old", dir);
    crate::eval::run(opts(&old, vec![Config::parse("varopt:16").unwrap()], vec![], vec![], None), input(&records), None, HashMap::new())
        .unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&old).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert_eq!(names, vec!["cost.tsv", "rows.tsv", "summary.tsv"]);
    for f in ["rows.tsv", "summary.tsv"] {
        assert_eq!(
            std::fs::read(format!("{}/{}", old, f)).unwrap(),
            std::fs::read(format!("{}/{}", out, f)).unwrap(),
            "{}: the side stores do not change the configs' rows",
            f
        );
    }
}

/// T-I1 (needs /data): the first 1,000,000 records, `count --kmv 1024` and
/// sixteen seeds of `--kmv 256` against the sets-counts reference: no hard
/// counter, and the seed gates of `count-seeds.tsv` -- seeds as the unit,
/// Bonferroni over the strata -- all pass.  One seed's per-stratum RMS is
/// not gated: within a run the errors share one permutation.
#[test]
#[ignore]
fn the_first_million_records_count_against_sets_counts() {
    let rec = "/data/bitcoin/2022/finalBCUTXO_2022.scm";
    let sc = "/data/bitcoin/2022/finalBCUTXO_2022.sets-counts.31187785.txt";
    let or = "/data/bitcoin/2022/lastspend-2022.u32";
    if !std::path::Path::new(rec).exists() {
        eprintln!("skipped: {} is not here", rec);
        return;
    }
    let dir = scratch("t-i1");
    let mut specs = vec![KmvSpec::parse("1024").unwrap()];
    specs.extend((1..=16).map(|s| KmvSpec::parse(&format!("256:seed={}", s)).unwrap()));
    let perms = count::perms_for(&specs);
    let n = 1_000_000u64;
    let oracle = Arc::new(Oracle::load(or, Some(n)).unwrap());
    let mut opts = count_opts(&specs, &dir, Some(sc.to_string()));
    opts.limit = n;
    opts.every = None;
    opts.stream = false;
    opts.bucket = 250_000;
    count::run(opts, Box::new(std::fs::File::open(rec).unwrap()), Some(oracle), &perms).unwrap();
    let check = std::fs::read_to_string(format!("{}/count-check.tsv", dir)).unwrap();
    eprintln!("{}", check);
    for line in check.lines().skip(1) {
        let c: Vec<&str> = line.split('\t').collect();
        assert_eq!(c[3], n.to_string());
        assert_eq!(&c[5..9], &["0", "0", "0", "0"], "{}", line);
    }
    let seeds = std::fs::read_to_string(format!("{}/count-seeds.tsv", dir)).unwrap();
    eprintln!("{}", seeds);
    let mut tested = 0;
    for line in seeds.lines().skip(1) {
        let c: Vec<&str> = line.split('\t').collect();
        assert_ne!(c[10], "0", "bias gate: {}", line);
        assert_ne!(c[14], "0", "variance gate: {}", line);
        tested += (c[10] == "1") as usize;
    }
    assert!(tested >= 6, "{} strata tested", tested);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A k = 2 seed family: the theory formula diverges there (`count_se` is
/// +inf), so every stratum's variance gate is `NA` and none fails, while the
/// bias is still tested.  The unit test pins the rule; the `count` run shows
/// it end to end.
#[test]
fn a_k2_family_tests_its_bias_and_not_its_variance() {
    let mut st = count::SeedStrata::new(4);
    for i in 0..50u64 {
        let es: Vec<f64> = (0..4).map(|s| ((i * 4 + s) % 7) as f64 / 10.0 - 0.3).collect();
        st.add(1000 + i, 2, &es, f64::INFINITY);
    }
    let lines = st.lines(0.01);
    assert!(!lines.is_empty());
    for l in &lines {
        assert!(l.bias_ok.is_some(), "{}: the bias is tested", l.stratum);
        assert_eq!(l.var_ok, None, "{}: no variance gate", l.stratum);
        assert!(l.var_ratio.is_nan());
    }

    let records = stream(3000, 400, 91);
    let dir = scratch("k2");
    let refs = format!("{}/sc.txt", dir);
    sets_counts(&records, &refs);
    let specs: Vec<KmvSpec> = (1..=4).map(|s| spec(2, s, Support::Sets)).collect();
    let perms = count::perms_for(&specs);
    count::run(count_opts(&specs, &dir, Some(refs)), input(&records), None, &perms).unwrap();
    let seeds = std::fs::read_to_string(format!("{}/count-seeds.tsv", dir)).unwrap();
    let mut tested = 0;
    for line in seeds.lines().skip(1) {
        let c: Vec<&str> = line.split('\t').collect();
        if c[2] == "<=k" {
            continue;
        }
        assert_eq!(c[14], "NA", "k = 2 has no variance gate: {}", line);
        tested += (c[10] != "NA") as usize;
    }
    assert!(tested > 0, "some k = 2 strata have their bias tested:\n{}", seeds);
}

/// The new modes refuse what they would ignore and what would collide: an
/// option `count` does not consume (`--l1`, `--config`, `--exact`), and a
/// store given twice to `pick` or `eval` (two threads, one file).
#[test]
fn new_modes_refuse_ignored_options_and_repeated_stores() {
    let argv = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    for (cmd, want) in [
        ("count --kmv 64 --l1 64 --out /nonexistent", "--l1"),
        ("count --l1 1024 --out /nonexistent", "--l1"),
        ("count --kmv 64 --config varopt:16 --out /nonexistent", "--config"),
        ("count --kmv 64 --exact --out /nonexistent", "--exact"),
        ("pick --select x --kmv 64 --sets-counts y --out /nonexistent", "--sets-counts"),
        ("pairs --in x --kmv 64 --out /nonexistent", "--kmv"),
        ("pairs --in x --in y --out /nonexistent", "one --in"),
        ("summary --in x --kmv 64 --out /nonexistent", "--kmv"),
        ("summary --in x --select y --out /nonexistent", "--select"),
        ("summary --in x --oracle y --out /nonexistent", "--oracle"),
    ] {
        let e = crate::run(crate::parse(&argv(cmd)).unwrap()).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains(want), "{}: {}", cmd, e);
    }
    assert!(crate::parse(&argv("summary --range 9:3 --out /nonexistent")).unwrap_err().contains("--range"));
    let dir = scratch("dup");
    let sel = format!("{}/sel.txt", dir);
    std::fs::write(&sel, "0\n").unwrap();
    let records = stream(10, 5, 3);
    let k = [spec(8, 1, Support::Sets), spec(8, 1, Support::Sets)];
    let l = [L1Spec::parse("64").unwrap(), L1Spec::parse("64").unwrap()];
    let pick_opts = |kmv: &[KmvSpec], l1: &[L1Spec], configs: Vec<Config>| PickOpts {
        select: sel.clone(),
        out: format!("{}/pick", dir),
        limit: u64::MAX,
        configs,
        kmv: kmv.to_vec(),
        l1: l1.to_vec(),
        exact: false,
        stats: false,
    };
    let v = Config::parse("varopt:16").unwrap();
    for o in [pick_opts(&k, &[], vec![]), pick_opts(&[], &l, vec![]), pick_opts(&[], &[], vec![v, v])] {
        let e = pick::run(o, input(&records), None, &HashMap::new(), &count::perms_for(&k)).unwrap_err();
        assert!(e.to_string().contains("given twice"), "{}", e);
    }
    for (kmv, l1) in [(k.to_vec(), vec![]), (vec![], l.to_vec())] {
        let o = crate::eval::EvalOpts {
            configs: vec![],
            out: format!("{}/eval", dir),
            limit: u64::MAX,
            every: 7,
            metric_threads: 1,
            check_ref: None,
            stats: false,
            perms: count::perms_for(&kmv),
            kmv,
            l1,
            l1_every: 7,
            sets_counts: None,
        };
        let e = crate::eval::run(o, input(&records), None, HashMap::new()).unwrap_err();
        assert!(e.to_string().contains("given twice"), "{}", e);
    }
    assert!(!std::path::Path::new(&format!("{}/pick", dir)).exists(), "refused before any output");
}

/// `eval --check-ref` with no `--config` still compares the exact colours
/// with the Laplace reference: a reference that is wrong on every sampled
/// record fails the run.  Before, the comparison ran only beside a config.
#[test]
fn check_ref_runs_without_a_config() {
    let records = stream(600, 100, 13);
    let dir = scratch("checkref");
    let mut txs = Vec::new();
    read_batches(Cursor::new(records.as_bytes()), u64::MAX, |b| {
        txs.extend(b.recs.iter().map(|r| r.tx));
        true
    })
    .unwrap();
    let text: String = txs.iter().map(|t| format!("{}\t2\t2\t2\t2\t2\t2\t2\n", t)).collect();
    let plain = format!("{}/lap.txt", dir);
    std::fs::write(&plain, text).unwrap();
    let st = std::process::Command::new("zstd").args(["-q", "-f", &plain, "-o", &format!("{}.zst", plain)]).status().unwrap();
    assert!(st.success());
    let kmv = vec![spec(16, 1, Support::Sets)];
    let o = crate::eval::EvalOpts {
        configs: vec![],
        out: format!("{}/eval", dir),
        limit: u64::MAX,
        every: 7,
        metric_threads: 1,
        check_ref: Some(format!("{}.zst", plain)),
        stats: false,
        perms: count::perms_for(&kmv),
        kmv,
        l1: vec![],
        l1_every: 7,
        sets_counts: None,
    };
    let e = crate::eval::run(o, input(&records), None, HashMap::new()).unwrap_err();
    assert!(e.to_string().contains("Laplace values differ from the reference"), "{}", e);
}

/// The `pos` reference is reachability through positive shares, not the
/// nonzero terms.  Block 1000's coin is diluted 1e18-fold at each of 30
/// steps, so its f64 coefficient underflows to 0 although every share on its
/// path is positive; a zero-amount input (block 3000) is the honest way out
/// of `pos`.  The exact store's `pos` support is the brute-force one on
/// every record, and `eval` and `pick`/`pairs` judge the `pos` KMV by it.
#[test]
fn an_underflowed_coefficient_stays_in_the_pos_support() {
    let mut records = record(1000, 0, &[], 1);
    records += &record(2000, 1, &[], 40);
    records += &record(3000, 2, &[], 1);
    let mut prev = 0;
    for tx in 3..33 {
        records += &record(4000 + tx, tx, &[(prev, 1), (1, 1_000_000_000_000_000_000)], 1);
        prev = tx;
    }
    records += &record(5000, 33, &[(prev, 1), (2, 0)], 1);
    let (terms, _) = fold_all(Exact::new(), &records, None, |c, alg| (alg.terms(c), alg.off(c)));
    let (sets, _) = fold_all(SetAlg { pos: false }, &records, None, |c, _| (**c).clone());
    let (pos, _) = fold_all(SetAlg { pos: true }, &records, None, |c, _| (**c).clone());
    for (i, ((t, off), (s, p))) in terms.iter().zip(sets.iter().zip(&pos)).enumerate() {
        let all: BTreeSet<u32> = t.iter().map(|x| x.0).collect();
        let reach: BTreeSet<u32> = all.iter().copied().filter(|b| off.binary_search(b).is_err()).collect();
        assert_eq!((&all, &reach), (s, p), "record {}", i);
    }
    // The coefficient did underflow, and the nonzero terms miss block 1000.
    let (last, off) = &terms[33];
    assert_eq!(last.iter().find(|x| x.0 == 1000).unwrap().1, 0.0);
    assert_eq!(*off, vec![3000]);
    assert_eq!(last.iter().filter(|x| x.1 != 0.0).count(), 1);
    assert_eq!(pos[33].len(), 2);

    let dir = scratch("underflow");
    let kspecs = vec![spec(2, 1, Support::Sets), spec(2, 1, Support::Pos), spec(4, 1, Support::Pos)];
    let o = crate::eval::EvalOpts {
        configs: vec![],
        out: format!("{}/eval", dir),
        limit: u64::MAX,
        every: 1,
        metric_threads: 2,
        check_ref: None,
        stats: false,
        perms: count::perms_for(&kspecs),
        kmv: kspecs.clone(),
        l1: vec![],
        l1_every: 1,
        sets_counts: None,
    };
    crate::eval::run(o, input(&records), None, HashMap::new()).unwrap();
    let rows = std::fs::read_to_string(format!("{}/eval/kmv-rows.tsv", dir)).unwrap();
    let last: Vec<&str> = rows.lines().filter(|l| l.starts_with("33\t")).collect();
    assert_eq!(last.len(), 3);
    for l in last {
        let c: Vec<&str> = l.split('\t').collect();
        assert_eq!((c[3], c[4]), ("3", "2"), "n_sets and n_pos: {}", l);
    }

    let sel = format!("{}/sel.txt", dir);
    std::fs::write(&sel, "31\n32\n33\n").unwrap();
    let out = format!("{}/pick", dir);
    let po = PickOpts {
        select: sel,
        out: out.clone(),
        limit: u64::MAX,
        configs: vec![],
        kmv: kspecs.clone(),
        l1: vec![],
        exact: true,
        stats: false,
    };
    pick::run(po, input(&records), None, &HashMap::new(), &count::perms_for(&kspecs)).unwrap();
    let ex = pick::read_exact(std::fs::File::open(format!("{}/exact.bin", out)).unwrap()).unwrap();
    assert_eq!(ex[2].1, vec![3000]);
    let pr = format!("{}/pairs", dir);
    crate::pairs::run(crate::pairs::PairsOpts { input: out, out: pr.clone(), groups: vec![], varopt_text: None, threads: 1 }).unwrap();
    let rows = std::fs::read_to_string(format!("{}/rows.all.tsv", pr)).unwrap();
    let h: Vec<&str> = rows.lines().next().unwrap().split('\t').collect();
    let (xn, xp) = (h.iter().position(|x| *x == "X_n").unwrap(), h.iter().position(|x| *x == "X_n_pos").unwrap());
    let r: Vec<&str> = rows.lines().nth(3).unwrap().split('\t').collect();
    assert_eq!((r[xn], r[xp]), ("3", "2"));
    let pairs = std::fs::read_to_string(format!("{}/pairs.all.tsv", pr)).unwrap();
    let h: Vec<&str> = pairs.lines().next().unwrap().split('\t').collect();
    let jp = h.iter().position(|x| *x == "X_J_pos").unwrap();
    // Records 32 and 33 have the same pos support, {1000, 2000}.
    let l = pairs.lines().find(|l| l.starts_with("1\t2\t")).unwrap();
    assert_eq!(l.split('\t').nth(jp).unwrap(), metrics::fmt(1.0));
}
