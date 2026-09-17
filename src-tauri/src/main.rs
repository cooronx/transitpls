//! 桌面端入口：启动 Tauri 应用。
// 防止 Windows 发行版弹出额外的控制台窗口，请勿删除。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    transitpls_lib::run()
}
