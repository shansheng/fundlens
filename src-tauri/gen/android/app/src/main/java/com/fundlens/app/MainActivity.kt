package com.fundlens.app

import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import java.io.File
import java.io.FileOutputStream

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    extractOcrAssets()
  }

  /**
   * 把 APK assets/ocr（det.mnn / rec.mnn / cls.mnn / dict.txt）解压到内部 dataDir/ocr。
   *
   * 为什么必需：Android 上打包资源经 tauri 的 resource_dir() 解析为 `asset://localhost/`（assets
   * 只能经 AssetManager 读取），Rust std::fs 无法直接访问；而 OCR 模型探测（src-tauri/src/ocr.rs
   * model_dir）第 3 分支会检查 app_data_dir()/ocr —— Android 上 app_data_dir() = dataDir（内部
   * 数据根目录，真实文件系统路径）。解压到 dataDir/ocr 后 Rust 无需任何改动即可读到模型。
   *
   * 用 versionName 写 .extracted_version 标记：应用升级（模型随包更新）时自动重解压，否则跳过
   * （冷启动零开销）。
   */
  private fun extractOcrAssets() {
    try {
      val names = assets.list("ocr") ?: return
      if (names.isEmpty()) return
      val destDir = File(applicationInfo.dataDir, "ocr")
      val version = try {
        packageManager.getPackageInfo(packageName, 0).versionName ?: "?"
      } catch (_: Exception) {
        "?"
      }
      val marker = File(destDir, ".extracted_version")
      if (marker.isFile && marker.readText() == version) return
      destDir.mkdirs()
      for (name in names) {
        assets.open("ocr/$name").use { input ->
          FileOutputStream(File(destDir, name)).use { output ->
            input.copyTo(output)
          }
        }
      }
      marker.writeText(version)
    } catch (_: Exception) {
      // 模型缺失/拷贝失败不阻断启动：截图导入走 OCR 时会向用户报明确错误
    }
  }
}
