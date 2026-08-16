// 预编译入口：在 macOS 上启动 Tauri 运行时
// Linux 一键构建脚本会直接使用 `tauri build`，此文件保证 `tauri dev`/`tauri build` 入口一致。

fn main() {
    fundlens_lib::run();
}
