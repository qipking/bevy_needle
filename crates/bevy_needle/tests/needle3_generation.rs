//! needle3 代际（v14.5：**needle2 已停止维护，Gen3 是唯一代际**）。
//!
//! 验证常量/库文件名/缓存分轨/base 权重接线；真实引擎的全链冒烟在
//! `needle3_real_smoke`（third_party/needle/3.0.1/ 有发行产物时跑）。

use bevy_needle::engine::{discover_library_for, EngineGeneration, ENGINE_VERSION};
use bevy_needle::prelude::*;
use std::path::PathBuf;

#[test]
fn library_name_has_generation_suffix() {
    if cfg!(target_os = "macos") {
        assert_eq!(EngineGeneration::Gen3.library_file_name(), "libneedle3.dylib");
    } else if cfg!(windows) {
        assert_eq!(EngineGeneration::Gen3.library_file_name(), "libneedle3.dll");
    } else {
        assert_eq!(EngineGeneration::Gen3.library_file_name(), "libneedle3.so");
    }
    assert_eq!(EngineGeneration::default(), EngineGeneration::Gen3);
}

#[test]
fn versions_and_cache_track() {
    assert_eq!(EngineGeneration::Gen3.engine_version(), "3.0.1");
    assert_eq!(ENGINE_VERSION, "3.0.1");
    assert_eq!(EngineGeneration::Gen3.cache_dir_name(), Some("v3"));
    assert_eq!(
        EngineGeneration::Gen3.base_weights_file_name(),
        Some("needle3.cact")
    );
}

#[test]
fn discovery_uses_generation_cache_track() {
    // 显式路径缺失 → 响亮报错（权威语义不变）
    let err = discover_library_for(
        EngineGeneration::Gen3,
        Some(std::path::Path::new("/nonexistent/needle3")),
    )
    .unwrap_err();
    assert!(err.to_string().contains("/nonexistent/needle3"));

    // 无 HOME 注入时发现失败但尝试列表可诊断（沙箱没有 gen3 库）
    let _ = discover_library_for(EngineGeneration::Gen3, None);
}

#[test]
fn open_missing_library_fails_loudly() {
    // dlopen 打开不存在的库 → 响亮失败（不静默回退）
    let err = bevy_needle::backend::DlopenBackend::open(
        std::path::Path::new("/nonexistent/libneedle3.so"),
        65536,
    )
    .unwrap_err();
    assert!(err.to_string().contains("nonexistent") || err.to_string().contains("dlopen"));
}

#[test]
fn engine_config_defaults_and_builders() {
    let config = EngineConfig::with_library("libneedle3.so");
    assert_eq!(config.base_weights_path, None);
    let config = config.with_base_weights("/tmp/needle3.cact");
    assert_eq!(
        config.base_weights_path.as_deref(),
        Some(std::path::Path::new("/tmp/needle3.cact"))
    );
}
