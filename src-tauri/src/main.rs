// 预编译入口：在 macOS 上启动 Tauri 运行时
// Linux 一键构建脚本会直接使用 `tauri build`，此文件保证 `tauri dev`/`tauri build` 入口一致。

fn main() {
    // 麒麟 V10 SP1（Mali 私有 GPU 驱动）下 WebKit2GTK 白屏修复：
    // 窗口/WebView 创建前必须生效，故置于 run() 之前。
    //  - WEBKIT_DISABLE_COMPOSITING_MODE：关闭 WebKit 合成器（Mali 驱动在 GTK 合成路径下白屏最常见根因）
    //  - WEBKIT_DISABLE_DMABUF_RENDERER：关闭 DMA-BUF 渲染器（私有驱动的 DRM/dmabuf 路径不稳定）
    // 对 macOS(WKWebView)/Windows(WebView2) 无害（不读取这些变量）；麒麟上消除「窗口出来但内容空白」。
    std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");

    // 麒麟 V10 SP1 2403 为 Wayland 会话：GTK3 的 GtkFileChooserNative 在 Wayland 下
    // 强制走 xdg-desktop-portal（GTK3.24 行为），系统未装/未运行 portal 后端时
    // 文件对话框（截图导入/备份导入导出）静默弹不出（2026-08-25 麒麟实测）。
    // 修复：检测到 XWayland 可用（/tmp/.X11-unix/X0 存在）时强制 GDK_BACKEND=x11，
    // GTK 文件框走传统 X11 对话框（XWayland 提供 X 服务），不依赖 portal。
    // XWayland 不可用时保持默认（Wayland+portal，需系统安装 xdg-desktop-portal）。
    if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland")
        && std::path::Path::new("/tmp/.X11-unix/X0").exists()
    {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    fundlens_lib::run();
}
