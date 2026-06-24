//! The `config.toml` load/save codec.
//!
//! The generic, domain-agnostic primitives that read and write DM420's interim
//! TOML config file: where it lives ([`config_path`]), the best-effort file
//! writer ([`write_config`]), the minimal table reader ([`parse_table_value`] /
//! [`parse_float`]) and the comment-preserving table writer ([`update_toml_table`]),
//! plus the small value formatters/parsers ([`format_f32`], [`bool_str`],
//! [`parse_u16`]). The domain mappings (which tables/keys back which settings)
//! live in [`crate::settings`], which delegates to these. **Not** a full TOML
//! parser — it deliberately avoids a dependency for a format that is still TBD
//! (see `joels-notes.md`); swap in `toml`/`toml_edit` when the config grows.

use std::path::{Path, PathBuf};

/// Where DM420's TOML config lives: `$HOME/.dm420/config.toml`, falling back to
/// `config.toml` in the current directory when there's no home. Holds the
/// `[station]` and `[audio]` tables (the latter also carries `tx_gain`, the
/// linear TX drive level — hand-edited, no env var) and the `[logging] level`.
/// The writers
/// (`Station::save`, [`save_audio_config`]) create the parent directory on first
/// save. The format/persistence is interim and TBD — see `joels-notes.md`.
pub(crate) fn config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".dm420").join("config.toml");
    }
    PathBuf::from("config.toml")
}

/// Create the config directory (`$HOME/.dm420`) if it doesn't exist yet, then
/// write `text` to `path`. Logs on error rather than failing — a config write is
/// best-effort.
pub(crate) fn write_config(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, text) {
        tracing::warn!(path = %path.display(), error = %e, "could not write config");
    }
}

/// Read a single string value from `table`'s `key`. **Not** a full TOML parser —
/// it deliberately avoids a dependency for a format that is still TBD (see
/// `joels-notes.md`); swap in the `toml` crate when the config grows. An empty
/// value counts as unset.
pub(crate) fn parse_table_value(text: &str, table: &str, key: &str) -> Option<String> {
    let mut in_table = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(t) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_table = t.trim() == table;
            continue;
        }
        if in_table
            && let Some((k, val)) = line.split_once('=')
            && k.trim() == key
        {
            let val = val.trim().trim_matches('"').trim();
            return (!val.is_empty()).then(|| val.to_string());
        }
    }
    None
}

/// Rewrite TOML `text` so `table` carries `kvs` (`key`, `value` pairs),
/// **preserving comments** and every other line: existing keys are updated in
/// place (inline comments kept), missing keys are appended to the table, and the
/// `[table]` is created if absent — leaving any other tables untouched. A real
/// `toml_edit` swap-in would subsume this (see `joels-notes.md`).
pub(crate) fn update_toml_table(text: &str, table: &str, kvs: &[(&str, &str)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_table = false;
    let mut seen = false;
    let mut written = vec![false; kvs.len()];
    let mut insert_at: Option<usize> = None; // after the last meaningful [table] line

    for raw in text.lines() {
        let code = raw.split('#').next().unwrap_or("").trim();
        if let Some(t) = code.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_table = t.trim() == table;
            seen |= in_table;
            out.push(raw.to_string());
            if in_table {
                insert_at = Some(out.len());
            }
            continue;
        }
        if in_table {
            let key = code.split_once('=').map(|(k, _)| k.trim());
            match key.and_then(|k| kvs.iter().position(|(kk, _)| *kk == k)) {
                Some(i) => {
                    out.push(rewrite_kv(raw, kvs[i].1));
                    written[i] = true;
                }
                None => out.push(raw.to_string()),
            }
            if !raw.trim().is_empty() {
                insert_at = Some(out.len());
            }
            continue;
        }
        out.push(raw.to_string());
    }

    let mut missing = Vec::new();
    for (i, (k, v)) in kvs.iter().enumerate() {
        if !written[i] {
            missing.push(format!("{k} = \"{v}\""));
        }
    }
    if !missing.is_empty() {
        if let (true, Some(at)) = (seen, insert_at) {
            for (i, line) in missing.into_iter().enumerate() {
                out.insert(at + i, line);
            }
        } else {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("[{table}]"));
            out.extend(missing);
        }
    }

    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Parse a numeric value from `table`'s `key` (stored as a quoted string, like
/// every other config value — see [`parse_table_value`]).
pub(crate) fn parse_float(text: &str, table: &str, key: &str) -> Option<f32> {
    parse_table_value(text, table, key).and_then(|v| v.parse::<f32>().ok())
}

/// Format a size/share for the config file: one decimal place, trimmed — keeps
/// the file readable without spurious float noise (`612.0`, not `612.0000305`).
pub(crate) fn format_f32(v: f32) -> String {
    let s = format!("{v:.1}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Rewrite a `key = value` line with a new quoted value, preserving the key, its
/// spacing, and any trailing inline comment.
fn rewrite_kv(raw: &str, new_val: &str) -> String {
    let Some(eq) = raw.find('=') else {
        return raw.to_string();
    };
    let prefix = &raw[..=eq];
    let post = &raw[eq + 1..];
    match post.find('#') {
        Some(h) => format!("{prefix} \"{new_val}\"  {}", post[h..].trim_end()),
        None => format!("{prefix} \"{new_val}\""),
    }
}

/// `"true"`/`"false"` for a config bool — the string form `[serial] autodetect`
/// and `[display] dark` are written/read as.
pub(crate) fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// Parse a `u16` written as `0x10C4` (hex) or plain decimal.
pub(crate) fn parse_u16(s: &str) -> Option<u16> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u16::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_ids_parse_hex_and_decimal() {
        assert_eq!(parse_u16("0x10C4"), Some(0x10C4));
        assert_eq!(parse_u16("0X10c4"), Some(0x10C4));
        assert_eq!(parse_u16("4292"), Some(4292));
        assert_eq!(parse_u16("nope"), None);
    }
}
