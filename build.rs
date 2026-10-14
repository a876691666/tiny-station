//! Build script: makes sure `web/dist` exists so `rust-embed` can always read
//! it, even in a checkout where the frontend has not been built yet. A real
//! `pnpm build` output is left untouched.

use std::path::Path;

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>tiny-station</title></head>
<body style="font-family:monospace;background:#0f1115;color:#d7dce2;padding:40px">
<h2>管理界面未构建</h2>
<p>请在 <code>web/</code> 目录执行 <code>pnpm install &amp;&amp; pnpm build</code> 后重新编译,或在开发时用 <code>pnpm dev</code>。</p>
</body></html>"#;

fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=build.rs");
    let dist = Path::new("web/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        let _ = std::fs::create_dir_all(dist);
        let _ = std::fs::write(&index, PLACEHOLDER);
    }
}
