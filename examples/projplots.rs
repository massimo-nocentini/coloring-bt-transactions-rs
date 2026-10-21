//! # A whole-chain output, reduced to what a plot loads
//!
//! The projections print one line a transaction, 778,613,440 of them over the
//! 2022 chain, and a plotting script cannot read that many lines in the time it
//! takes to draw.  This reads one such file from stdin in 64 MB chunks cut at
//! line ends, parses the chunks on a pool of workers, and writes what the
//! figure needs: either the columns as `f64` arrays, or the bands binned onto a
//! fixed canvas.
//!
//! ```text
//! zstd -dc colors.laplace.zst | projplots cols 1 7 laplace.f64   # 43.6 GB
//! projplots cols 0 1 mean.f64 < colors.mean.txt                   #  6.2 GB
//! zstd -dc --long=27 colors.bands1024.zst \
//! | projplots bands 778613440 2048 1024 bands1024
//! ```
//!
//! - `cols <skip> <take> <out>` -- tab-separated lines; skip `skip` fields,
//!   parse the next `take` as `f64`, and write them row-major at
//!   `line * take * 8` in `<out>`, so a worker writes its chunk where it goes
//!   and nothing waits on order.  The two `cols` runs above took four and a
//!   half minutes together, and their arrays are what
//!   `figures/laplace/reduce_laplace.py` in the article reads.
//! - `bands <records> <rows> <bands> <prefix>` -- `tx \t w:k,w:k,...` lines,
//!   as `--bands` prints them.  Line `i` goes to row `i * rows / records`, and
//!   each row sums `w` per band and counts the lines whose support holds band
//!   `k`.  Writes `<prefix>.sum.f64` and `<prefix>.support.u64`, `rows x
//!   bands`, and `<prefix>.lines.u64`, one count a row.  Every row of the sum
//!   divided by its line count is a colour and sums to 1, which is the check.
//!   Workers take chunks in whatever order they free up, so the sums agree
//!   between runs to rounding (1e-17 relative over 3,000,000 lines) and the
//!   counts exactly.
//!   The bottleneck is `zstd -dc`, single-threaded at about 0.9 GB/s, so the
//!   8.67 TiB of the `--bands 1024` file is a three-hour pass.
//!
//! Rows are record ordinals, not transaction ids: from record 142,842 on the
//! two BIP-30 duplicate coinbases put the ordinal ahead of the id.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::FileExt;
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::thread;

const CHUNK: usize = 64 << 20;

/// Read stdin in chunks that end on a newline, numbering their first lines,
/// and hand them to `work` on `threads` workers.  Returns the line count and
/// every worker's result.
fn stream<T: Send + 'static>(
    threads: usize,
    make: impl Fn() -> T + Send + Sync + 'static,
    work: impl Fn(&mut T, u64, &[u8]) + Send + Sync + 'static,
) -> (u64, Vec<T>) {
    let (tx, rx) = sync_channel::<(u64, Vec<u8>)>(threads * 2);
    let rx = Arc::new(Mutex::new(rx));
    let make = Arc::new(make);
    let work = Arc::new(work);
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let rx = rx.clone();
            let make = make.clone();
            let work = work.clone();
            thread::spawn(move || {
                let mut state = make();
                loop {
                    let next = rx.lock().unwrap().recv();
                    match next {
                        Ok((start, buf)) => work(&mut state, start, &buf),
                        Err(_) => break,
                    }
                }
                state
            })
        })
        .collect();

    let mut stdin = std::io::stdin().lock();
    let mut carry: Vec<u8> = Vec::new();
    let mut line: u64 = 0;
    let mut done = false;
    let mut last_report = 0u64;
    while !done {
        let mut buf = std::mem::take(&mut carry);
        let have = buf.len();
        buf.resize(have + CHUNK, 0);
        let mut filled = have;
        while filled < buf.len() {
            let n = stdin.read(&mut buf[filled..]).expect("read stdin");
            if n == 0 {
                done = true;
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        let cut = if done {
            if buf.last().is_some_and(|&b| b != b'\n') {
                buf.push(b'\n');
            }
            buf.len()
        } else {
            match buf.iter().rposition(|&b| b == b'\n') {
                Some(p) => p + 1,
                None => {
                    carry = buf;
                    continue;
                }
            }
        };
        carry = buf[cut..].to_vec();
        buf.truncate(cut);
        let lines = buf.iter().filter(|&&b| b == b'\n').count() as u64;
        if !buf.is_empty() {
            tx.send((line, buf)).unwrap();
        }
        line += lines;
        if line - last_report >= 10_000_000 {
            last_report = line;
            eprintln!("{} lines", line);
        }
    }
    drop(tx);
    let states = handles.into_iter().map(|h| h.join().unwrap()).collect();
    (line, states)
}

fn lines(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    buf.split(|&b| b == b'\n').filter(|l| !l.is_empty())
}

fn parse<T: std::str::FromStr>(bytes: &[u8]) -> T
where
    T::Err: std::fmt::Debug,
{
    std::str::from_utf8(bytes).unwrap().parse().unwrap()
}

fn cols(skip: usize, take: usize, out: &str) {
    let file = Arc::new(File::create(out).expect("create output"));
    let threads = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
        .min(32);
    let f = file.clone();
    let (n, _) = stream(
        threads,
        || (),
        move |_, start, buf| {
            let mut values: Vec<f64> = Vec::with_capacity(buf.len() / 8);
            for l in lines(buf) {
                let mut fields = l.split(|&b| b == b'\t').skip(skip);
                for _ in 0..take {
                    values.push(parse(fields.next().expect("too few fields")));
                }
            }
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            f.write_all_at(&bytes, start * take as u64 * 8)
                .expect("write");
        },
    );
    eprintln!("lines {}", n);
    println!("{}", n);
}

struct Acc {
    sum: Vec<f64>,
    support: Vec<u64>,
    lines: Vec<u64>,
}

fn bands(records: u64, rows: usize, bands: usize, prefix: &str) {
    let threads = 24;
    let (n, accs) = stream(
        threads,
        move || Acc {
            sum: vec![0.0; rows * bands],
            support: vec![0; rows * bands],
            lines: vec![0; rows],
        },
        move |acc, start, buf| {
            for (i, l) in lines(buf).enumerate() {
                let line = start + i as u64;
                let row = ((line as u128 * rows as u128 / records as u128) as usize).min(rows - 1);
                acc.lines[row] += 1;
                let tab = l.iter().position(|&b| b == b'\t').expect("tab");
                let body = &l[tab + 1..];
                if body.is_empty() {
                    continue;
                }
                let base = row * bands;
                for term in body.split(|&b| b == b',') {
                    let colon = term.iter().position(|&b| b == b':').expect("w:k");
                    let w: f64 = parse::<f32>(&term[..colon]) as f64;
                    let k: usize = parse(&term[colon + 1..]);
                    acc.sum[base + k] += w;
                    if w != 0.0 {
                        acc.support[base + k] += 1;
                    }
                }
            }
        },
    );
    let mut sum = vec![0.0f64; rows * bands];
    let mut support = vec![0u64; rows * bands];
    let mut lines_per_row = vec![0u64; rows];
    for a in accs {
        for (s, x) in sum.iter_mut().zip(&a.sum) {
            *s += x;
        }
        for (s, x) in support.iter_mut().zip(&a.support) {
            *s += x;
        }
        for (s, x) in lines_per_row.iter_mut().zip(&a.lines) {
            *s += x;
        }
    }
    let write = |name: &str, bytes: Vec<u8>| {
        File::create(format!("{}.{}", prefix, name))
            .unwrap()
            .write_all(&bytes)
            .unwrap();
    };
    write(
        "sum.f64",
        sum.iter().flat_map(|v| v.to_le_bytes()).collect(),
    );
    write(
        "support.u64",
        support.iter().flat_map(|v| v.to_le_bytes()).collect(),
    );
    write(
        "lines.u64",
        lines_per_row.iter().flat_map(|v| v.to_le_bytes()).collect(),
    );
    eprintln!("lines {} (expected {})", n, records);
    println!("{}", n);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("cols") => cols(
            parse(args[2].as_bytes()),
            parse(args[3].as_bytes()),
            &args[4],
        ),
        Some("bands") => bands(
            parse(args[2].as_bytes()),
            parse(args[3].as_bytes()),
            parse(args[4].as_bytes()),
            &args[5],
        ),
        _ => {
            eprintln!("usage: projplots cols <skip> <take> <out> | bands <records> <rows> <bands> <prefix>");
            std::process::exit(2);
        }
    }
}
