use std::{env, fs, path::Path};

fn main() {
    // Ensure ../web/dist exists so rust-embed has a folder to embed/serve,
    // even when the frontend has not been built yet.
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let dist = Path::new(&manifest).join("../web/dist");
    if !dist.exists() {
        fs::create_dir_all(&dist).expect("create web/dist placeholder");
        fs::write(
            dist.join("index.html"),
            "<!doctype html><html lang=\"ja\"><head><meta charset=\"utf-8\"><title>Kenji Console</title></head><body style=\"background:#09090b;color:#e4e4e7;font-family:sans-serif;display:flex;align-items:center;justify-content:center;height:100vh\"><div style=\"text-align:center\"><h1>Kenji</h1><p>Web UI not built yet. Run <code>npm install &amp;&amp; npm run build</code> in the <code>web/</code> directory, then restart the server.</p></div></body></html>",
        )
        .expect("write placeholder index");
        println!("cargo:warning=created placeholder web/dist/index.html");
    }
    println!("cargo:rerun-if-changed=../web/dist/index.html");
}
