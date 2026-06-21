# Flag icons

Country flag PNGs (`<iso>.png`, keyed by lowercase ISO 3166-1 alpha-2) sourced
from **flagpedia.net** / **flagcdn.com** (`https://flagcdn.com/w160/<iso>.png`).

Flag images are in the **public domain**; flagpedia distributes them freely with
no attribution required. Fetched at the `w160` size and embedded into the `dm420`
binary at build time (see `crates/gui/build.rs`) so the Call Sign panel renders
real flags fully offline.

To refresh or add a country, drop a `<iso>.png` into this directory and rebuild.
