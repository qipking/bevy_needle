# Vendor: univis_ui

本目录内嵌 [univis_ui](https://github.com/univiseditor/univis_ui) v0.3.1 源码副本
（MIT License，原作者 abdellah-haffout），供 `examples/univis_needle_demo` 使用。

**为何 vendor 而不用 crates.io 版本**：demo 依赖 4 处本地修复（尚未进入上游）——

1. `toggle.rs` `animate_toggle_knob`：snap 分支改为条件写，避免每帧标脏组件
   （增量渲染下表现为整面板闪烁）；
2. `toggle.rs` `emit_toggle_events`：回写 `previous_checked`，外部直接改
   `checked` 时事件不再每帧重发；
3. `select/visuals.rs`：根节点属性改为变化才写，同因（每帧脏写）；
4. `schedule.rs`：`UiRolloutConfig::default` 关闭 `use_incremental_render`
   （增量渲染 frontier 世代判定存在隔帧重绘缺陷，见 docs/architecture.md）。

上游修复后可切回 registry 版本并删除本目录。
