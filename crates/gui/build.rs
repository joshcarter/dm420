//! Embeds the bundled country-flag PNGs (`assets/flags/<iso>.png`) into the
//! binary so the Call Sign panel renders real flags fully offline. Generates a
//! `FLAGS: &[(&str, &[u8])]` table of (lowercase ISO, PNG bytes), keyed the same
//! way `flag.rs` looks them up. No-op (empty table) if the directory is absent.

use std::{env, fs, path::Path};

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let flags_dir = Path::new(&manifest).join("assets/flags");
    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("flags_data.rs");

    let mut rows = String::new();
    if let Ok(read) = fs::read_dir(&flags_dir) {
        let mut pngs: Vec<String> = read
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "png"))
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
            .collect();
        pngs.sort();
        for iso in pngs {
            rows.push_str(&format!(
                "    (\"{iso}\", include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/assets/flags/{iso}.png\"))),\n"
            ));
        }
    }

    fs::write(
        &dest,
        format!("pub static FLAGS: &[(&str, &[u8])] = &[\n{rows}];\n"),
    )
    .unwrap();
    println!("cargo:rerun-if-changed=assets/flags");
}
