//! `ab_jt9` — Half B of the decoder measurement harness.
//!
//! Run our decoder and WSJT-X's `jt9` on the *same* captured slot(s) and diff the
//! two message sets: **matched / ours-only / jt9-only**. "jt9-only" is the gap —
//! the transmissions WSJT-X pulls out that we don't — reported with each miss's
//! SNR and frequency so you can see *which* signals we lose (weak? crowded?).
//!
//! This is the absolute calibration for the synthetic `crowd_recall` finding: it
//! confirms (or refutes) on real off-air audio that our busy-band losses are the
//! same masking story, not a synthetic artifact.
//!
//! Capture input with WSJT-X **Settings → Save → "Save all"**: those WAVs are
//! 12 kHz / 16-bit / mono / 15 s and named `YYMMDD_HHMMSS.wav`, exactly what both
//! `jt9` and our decoder expect — no conversion.
//!
//! Run:
//!   cargo run -p modes --example ab_jt9 -- ~/Library/Application\ Support/WSJT-X/save/*.wav
//!   cargo run -p modes --example ab_jt9 -- /path/to/save_dir          # scans *.wav
//!
//! The `jt9` invocation is configurable for version drift:
//!   JT9_BIN=jt9            path to the binary (default `jt9`, found on PATH)
//!   JT9_ARGS="--ft8 -d 3"  args inserted before the WAV path (default shown)

use modes::{Protocol, decode};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A decode reduced to what we compare on: the normalized message is the key;
/// snr/freq ride along for reporting.
#[derive(Clone)]
struct Row {
    msg: String,
    snr: f32,
    freq: f32,
}

fn main() {
    let paths = collect_wavs(std::env::args().skip(1));
    if paths.is_empty() {
        eprintln!(
            "usage: ab_jt9 <slot.wav | save_dir> ...\n\
             (capture with WSJT-X 'Save all'; see file header for env knobs)"
        );
        std::process::exit(2);
    }

    let jt9_bin = std::env::var("JT9_BIN").unwrap_or_else(|_| "jt9".into());
    let jt9_args: Vec<String> = std::env::var("JT9_ARGS")
        .unwrap_or_else(|_| "--ft8 -d 3".into())
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    // Aggregate tallies across every file.
    let (mut tot_match, mut tot_ours_only, mut tot_jt9_only) = (0usize, 0usize, 0usize);

    for path in &paths {
        let (sig, sr) = read_wav_mono(path);
        if sr != 12_000 {
            eprintln!("⚠ {}: {sr} Hz (expected 12 kHz WSJT-X save) — skipping", path.display());
            continue;
        }

        let ours = normalize_rows(
            decode(&sig, sr, Protocol::Ft8)
                .into_iter()
                .map(|d| Row { msg: d.message, snr: d.snr_db, freq: d.freq_hz }),
        );
        let theirs = match run_jt9(&jt9_bin, &jt9_args, path) {
            Ok(rows) => normalize_rows(rows.into_iter()),
            Err(e) => {
                eprintln!("⚠ {}: jt9 failed: {e}", path.display());
                continue;
            }
        };

        let ours_keys: BTreeSet<&str> = ours.iter().map(|r| r.msg.as_str()).collect();
        let theirs_keys: BTreeSet<&str> = theirs.iter().map(|r| r.msg.as_str()).collect();

        let matched = ours_keys.intersection(&theirs_keys).count();
        let ours_only: Vec<&Row> =
            ours.iter().filter(|r| !theirs_keys.contains(r.msg.as_str())).collect();
        let jt9_only: Vec<&Row> =
            theirs.iter().filter(|r| !ours_keys.contains(r.msg.as_str())).collect();

        tot_match += matched;
        tot_ours_only += ours_only.len();
        tot_jt9_only += jt9_only.len();

        let gap_pct = pct(jt9_only.len(), theirs.len());
        println!("\n== {} ==", path.display());
        println!(
            "ours {:<3} jt9 {:<3} | matched {:<3} ours-only {:<3} jt9-only {:<3} → gap {} ({gap_pct:.0}%)",
            ours.len(),
            theirs.len(),
            matched,
            ours_only.len(),
            jt9_only.len(),
            jt9_only.len(),
        );
        if !jt9_only.is_empty() {
            println!("  jt9-only (we miss):");
            for r in sorted_by_freq(&jt9_only) {
                println!("    {:>+4.0} dB  {:>5.0} Hz  {}", r.snr, r.freq, r.msg);
            }
        }
        if !ours_only.is_empty() {
            println!("  ours-only (jt9 misses, or a false decode — check these):");
            for r in sorted_by_freq(&ours_only) {
                println!("    {:>+4.0} dB  {:>5.0} Hz  {}", r.snr, r.freq, r.msg);
            }
        }
    }

    let denom = tot_match + tot_jt9_only; // jt9's total = the reference count
    println!(
        "\n=== aggregate over {} file(s) ===\n\
         matched {tot_match}  ours-only {tot_ours_only}  jt9-only {tot_jt9_only}  → gap {tot_jt9_only}/{denom} = {:.0}%",
        paths.len(),
        pct(tot_jt9_only, denom),
    );
    if tot_ours_only > 0 {
        println!(
            "note: {tot_ours_only} ours-only decode(s) — each is either a real signal jt9 missed \
             or a false decode of ours; worth eyeballing."
        );
    }
}

/// Invoke `jt9` on one WAV and parse its stdout decode lines. Runs in the temp
/// dir (with an absolute WAV path) because `jt9` writes working files —
/// `decoded.txt`, `jt9_wisdom.dat`, `timer.out` — into its cwd; we don't want
/// those landing in the repo.
fn run_jt9(bin: &str, args: &[String], wav: &Path) -> Result<Vec<Row>, String> {
    let abs = std::fs::canonicalize(wav).unwrap_or_else(|_| wav.to_path_buf());
    let out = Command::new(bin)
        .args(args)
        .arg(&abs)
        .current_dir(std::env::temp_dir())
        .output()
        .map_err(|e| format!("spawn {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_jt9(&stdout);
    if rows.is_empty() && !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "exit {:?}; no decodes parsed. stderr:\n{}",
            out.status.code(),
            stderr.trim()
        ));
    }
    Ok(rows)
}

/// Parse `jt9` FT8 stdout. WSJT-X decode lines carry the FT8 mode marker `~`:
///   `HHMMSS  SNR  DT  FREQ ~  MESSAGE`   (a leading UTC may or may not be present)
/// Everything after `~` is the message; the numeric tokens before it end in
/// `… SNR DT FREQ`, so freq is the last token and snr the third-from-last.
fn parse_jt9(stdout: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in stdout.lines() {
        let Some((pre, rest)) = line.split_once('~') else {
            continue;
        };
        let msg = rest.trim();
        if msg.is_empty() {
            continue;
        }
        let toks: Vec<&str> = pre.split_whitespace().collect();
        if toks.len() < 3 {
            continue;
        }
        let freq = toks[toks.len() - 1].parse::<f32>().ok();
        let snr = toks[toks.len() - 3].parse::<f32>().ok();
        let (Some(freq), Some(snr)) = (freq, snr) else {
            continue;
        };
        if !(100.0..=6000.0).contains(&freq) {
            continue; // guard against stray non-decode lines
        }
        rows.push(Row { msg: normalize_msg(msg), snr, freq });
    }
    rows
}

/// Normalize a batch, de-duplicating on the message key (a repeated identical
/// message in one slot collapses — same as the per-slot dedup our decoder does).
fn normalize_rows(rows: impl Iterator<Item = Row>) -> Vec<Row> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for mut r in rows {
        r.msg = normalize_msg(&r.msg);
        if seen.insert(r.msg.clone()) {
            out.push(r);
        }
    }
    out
}

/// Uppercase + collapse whitespace so the same transmission compares equal
/// regardless of spacing differences between the two decoders.
fn normalize_msg(m: &str) -> String {
    m.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase()
}

fn sorted_by_freq<'a>(rows: &[&'a Row]) -> Vec<&'a Row> {
    let mut v = rows.to_vec();
    v.sort_by(|a, b| a.freq.partial_cmp(&b.freq).unwrap());
    v
}

fn pct(num: usize, denom: usize) -> f32 {
    if denom == 0 { 0.0 } else { 100.0 * num as f32 / denom as f32 }
}

/// Expand each argument: a `.wav` file is taken as-is; a directory is scanned
/// (non-recursively) for `*.wav`.
fn collect_wavs(args: impl Iterator<Item = String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for a in args {
        let p = PathBuf::from(&a);
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    let ep = e.path();
                    if ep.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")) {
                        out.push(ep);
                    }
                }
            }
        } else if p.is_file() {
            out.push(p);
        } else {
            eprintln!("⚠ not found: {a}");
        }
    }
    out.sort();
    out
}

/// Minimal canonical-WAV reader (16-bit PCM mono), mirroring `tests/fixtures_decode.rs`.
fn read_wav_mono(path: &Path) -> (Vec<f32>, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let mut i = 12;
    let (data_off, data_len) = loop {
        let id = &bytes[i..i + 4];
        let sz =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        if id == b"data" {
            break (i + 8, sz);
        }
        i += 8 + sz;
    };
    let samples = bytes[data_off..data_off + data_len]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();
    (samples, sample_rate)
}
