//! needle3 真机全链冒烟（Rust FFI 路径）。
//!
//! 前提：`third_party/needle/3.0.1/{libneedle3.so,needle3.cact}` 存在
//! （发行产物；缺失则整测跳过——CI 与无引擎环境不阻塞）。

use bevy_needle::engine::{discover_library_for, EngineGeneration};
use bevy_needle::ffi::FfiEngine;
use std::path::PathBuf;

fn gen3_artifacts() -> Option<(PathBuf, PathBuf)> {
    let lib = discover_library_for(EngineGeneration::Gen3, None).ok()?;
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/needle/3.0.1/needle3.cact");
    if lib.is_file() && base.is_file() {
        Some((lib, base))
    } else {
        None
    }
}

#[test]
fn needle3_real_smoke() {
    let Some((lib_path, base)) = gen3_artifacts() else {
        eprintln!("skip: third_party/needle/3.0.1 artifacts not present");
        return;
    };
    println!("lib = {}", lib_path.display());
    let ffi = FfiEngine::open(&lib_path).expect("open gen3");

    let blob = std::fs::read(&base).expect("read base weights");
    ffi.load_weights(&blob).expect("load base weights");

    ffi.init("you are a console assistant", "[]", None).expect("init");
    let mut buf = vec![0u8; 65536];
    let n = ffi.complete("hello", 64, &mut buf).expect("complete");
    let envelope = std::str::from_utf8(&buf[..n]).unwrap();
    println!("envelope = {envelope}");
    assert!(envelope.contains("\"type\""), "envelope shape");

    let vec = ffi.embed("hello").expect("embed");
    assert_eq!(vec.len(), 3072, "needle3 embedding dimension");
    ffi.reset();
}
