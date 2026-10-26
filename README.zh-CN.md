# Desk List

**贴在桌面壁纸上的待办清单。** 它待在桌面层级、压在所有窗口之下，平时鼠标完全穿透 —— 按住 `Ctrl` 才活。

[English](README.md) · 简体中文

<img src="assets/screenshot.png" width="440" alt="Desk List">

## 它是什么

一张永远在的小卡片，钉在桌面上。不占任务栏、不弹窗、不需要你「打开」它 —— 清单就在那里，挨着你的图标。

鼠标点它等于点到桌面，不挡任何操作。按住 `Ctrl`，它醒过来：分类页签、新建、已完成 / 已归档抽屉一起浮现。

## 功能

- **桌面的一部分** —— 压在 Z 序最底、鼠标穿透、不进任务栏也不进 `Alt+Tab`
- **按住 `Ctrl` 才可交互** —— 页签、新建、已完成与已归档抽屉
- **`Ctrl+Alt+D`** —— 切换「置顶态」：白卡常驻、可点击、始终在最前；再按一次回到桌面
- **`Ctrl+Alt+N`** —— 在任何程序里新建一条
- **托盘菜单** —— 显示、置顶 / 贴回桌面、退出
- **分类与归档** —— 勾掉一条进该分类的「已完成」，归档则清空
- **可调宽** —— 拖右边缘，320–520 px
- **本地 JSON 存档** —— 无账号、无同步、无遥测

## 下载

Windows 10 / 11 (x64)，见 [最新发布](https://github.com/WoBok/desklist/releases/latest)：

- `DeskList-Setup-x.y.z.exe` —— 安装版，装到当前用户，不需要管理员权限
- `DeskList-x.y.z-Portable.exe` —— 单文件绿色版，放哪都能跑

## 从源码构建

需要 Rust 1.77+、Node 18+，以及 [Tauri 2 的前置依赖](https://tauri.app/start/prerequisites/)。

```bash
npm install
npx tauri build            # → src-tauri/target/release/desklist.exe
```

安装包由 `installer/setup.nsi` 生成。

## 说明

- 以 Windows 为先。代码里留有 macOS 分支，但未实测。
- `tauri.conf.json` 里关掉了 GPU 合成（`--disable-gpu`）。这是开发机上让透明窗口正常出画的唯一办法，你的显卡没问题就去掉这两个参数。
- 闲置开销：约 31 MB 内存，CPU 长期低于 0.1%。
