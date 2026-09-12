# Satelite

<p align="center">
  <strong>卫星飞天，连接无限。</strong><br/>
  轻量、好看的 sing-box / Xray / mihomo 桌面客户端<br/>
</p>

<p align="center">
  <a href="https://github.com/zn0wii/satelite-proxy/stargazers"><img src="https://img.shields.io/github/stars/zn0wii/satelite-proxy?style=social" alt="Stars" /></a>
  &nbsp;
  <img src="https://img.shields.io/badge/macOS-Apple%20Silicon%20%7C%20Intel-111111?logo=apple&logoColor=white" alt="macOS" />
  <img src="https://img.shields.io/badge/Windows-x64-0078D4?logo=windows&logoColor=white" alt="Windows" />
  &nbsp;
  <img src="https://img.shields.io/badge/Linux-x64-FCC624?logo=linux&logoColor=black" alt="Linux" />
  <img src="https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white" alt="Tauri" />
  <img src="https://img.shields.io/badge/Rust-%23000000?logo=rust&logoColor=white" alt="Rust" />
  <img src="https://img.shields.io/badge/License-Apache%202.0-green.svg" alt="License" />
</p>

导入订阅、三内核切换、规则分流、代理链、智能 DNS、系统代理 / TUN、托盘常驻——日常该有的都有。  
它**足够轻、足够稳、也足够好看**。

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./assets/banner-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="./assets/banner-light.png">
    <img src="./assets/banner-light.png" alt="Banner">
  </picture>
</p>

## 为什么是 Satelite

代理客户端已经够多了。Satelite 不想再做一个「功能清单更长」的壳，而是把 **sing-box / Xray / mihomo** 三颗内核，收成一颗真正能放在桌面上的卫星：

| 你真正在意的 | Satelite 怎么做 |
| --- | --- |
| **体积与内存** | Tauri 2 + Rust，不是 Chromium 全家桶。开着托盘就该被忘掉，而不是占掉半条内存。关到托盘还可选「低内存模式」，把界面卸掉。 |
| **不想被一颗内核绑死** | sing-box（默认）、Xray、mihomo（Clash Meta）设置页一键切换，订阅、规则、DNS 全套配置跟着走。还能开「多核模式」：sing-box 主监听不动，按协议把指定节点委托给 Xray / mihomo 副进程。 |
| **节点会挂** | 三种选路：手动、内核 urltest、应用侧智能切换。智能模式靠连接日志被动感知 + 按需探测，自动避障，而不是一直狂扫全表。 |
| **不想被配置淹没** | 「简洁模式」只留连接 / 节点 / 流量；「专业模式」打开规则、DNS、Hosts、日志。同一套内核，两套节奏。 |
| **界面也是功能** | 玻璃拟态、浅色 / 深色、多种主题色（也支持自定义取色）、首页动效三选一（粒子 / 经典 / 笑脸）。打开窗口的那一秒，就该知道这不是 2018 年的后台面板。 |
| **开箱即用** | 三颗内核自动下载更新，意外退出自动拉起；Clash 订阅、sing-box JSON、分享链接、`clash://` / `sing-box://` / `singbox://` 浏览器一键导入。 |

> 卫星绕着你转，而不是你围着 YAML 转。

---

## 它能做什么

- **订阅与配置**：Clash 订阅、sing-box JSON、节点分享链接；链接 / 文件 / 浏览器深链导入；订阅可定时更新。也可以把一份完整 sing-box 配置直接当运行时。
- **三内核自由切**：sing-box（默认）· Xray · mihomo（Clash Meta），设置页一键下载 / 更新 / 切换。再进一步，「多核模式」让 sing-box 主监听不动，按协议把节点委托给 Xray / mihomo 副进程，多颗内核同场干活。
- **协议**：SS、VMess、VLESS、Trojan、Hysteria2、TUIC、AnyTLS、WireGuard、SOCKS5 等；不支持的协议会按当前内核自动过滤，不会生成连不上的配置。
- **代理链**：节点池 + 多跳链式分流（入口 → 中转 → 落地），地铁线画布上拖拽编辑，一键逐跳诊断每段真实出口。
- **智能选路**：手动 · 应用智能避障 · 内核 urltest，按场景选，不绑死一种策略。
- **规则分流**：多规则集（本地 / 远程 `.srs`），拖拽优先级；策略可整组指定节点、关键词过滤或指向代理链；内置国内站点 / 国内 IP / 海外规则；兜底出口代理 / 直连 / 屏蔽。规则 / 全局 / 直连一键切换。
- **DNS 与 Hosts**：DoH / DoT / FakeIP，DNS 规则集、系统 Hosts、默认解析器；内置 DNS 诊断，逐域名推演解析路径，本地 / 国内路径标红提醒泄露风险。
- **网络探测**：首页一键测当前节点延迟 + 出口 IP，四个 IP 源竞速探测，节点切换后自动重探。
- **节点管理**：按订阅 / 协议 / 国家分组，搜索排序；TCP Ping 测可达性、真实延迟测整条链路、点测单节点即点即测，结果逐节点流式回传。
- **系统代理 / TUN**：系统代理一键接管；TUN（system / gvisor / mixed）；绕过局域网、可选 TUN IPv6、可拦 QUIC。
- **端口**：mixed / Clash API（访问密钥可选）、多监听、允许局域网。
- **连接与流量**：活跃连接、已关闭、失败请求、流量走向，自动解析进程名。
- **托盘常驻**：关窗进托盘，开机启动、静默启动、可选托盘图标；内核在后台，窗口可消失。
- **内核自管**：三颗内核自动拉取并更新，意外退出自动拉起，不用自己找二进制、对版本。
- **中英双语文案**，浅色 / 深色，多种主题色。

<p align="center">
  <img src="assets/1.png" alt="Windows 概览" width="760" />
  &nbsp;
  <img src="assets/3.png" alt="规则分流" width="760" />
</p>
<p align="center">
  <img src="assets/2.png" alt="应用设置" width="760" />
</p>

## 🖥 平台支持

| 平台            | 状态   |
| --------------- | ------ |
| macOS Apple 芯片 | ✅ 支持 |
| macOS Intel     | ✅ 支持 |
| Windows         | ✅ 支持 |
| Linux           | ✅ 支持 |
| Android         | 🧪 早期 · [Interstellar](https://github.com/zn0wii/interstellar)（独立项目，见下） |

> Satelite Proxy 仍在持续开发中，升级前请备份重要的配置文件。

### macOS 提示「已损坏，无法打开」

未签名应用会被 macOS 的 Gatekeeper 隔离标记拦下。终端执行以下命令后再打开即可：

```bash
sudo xattr -d com.apple.quarantine /Applications/Satelite.app
```

## 📱 Android 版：Interstellar

卫星正在飞向移动端——**[Interstellar](https://github.com/zn0wii/interstellar)** 是 Satelite 的 Android 版本。

需要说明的是：它虽定位为「移动版」，却不是桌面端的移植，而是一个**全新实现的独立项目**。Interstellar 复用了 Satelite 的部分设计理念，界面与架构均针对移动端从零重新设计。

Interstellar 目前仍处于早期阶段，功能与体验都在快速迭代，欢迎[围观](https://github.com/zn0wii/interstellar)、试用与反馈。

## 🛠 技术栈

- **内核**：[sing-box](https://github.com/SagerNet/sing-box)（默认）· [Xray](https://github.com/XTLS/Xray-core) · [mihomo](https://github.com/MetaCubeX/mihomo)（Clash Meta）
- **桌面框架**：[Tauri 2](https://tauri.app/)
- **前端**：React + TypeScript + Vite
- **后端**：Rust

## 📦 开发

```bash
# 安装依赖
pnpm install

# 启动开发模式（缺内核或内置规则集时，应用会自行下载）
pnpm tauri dev
```

打包脚本默认把三颗内核（sing-box / Xray / mihomo，含各自 geodata）与三条内置远程规则集（`.srs`）一并打进安装包，缺失时自动下载；`--singbox-only` / `-SingboxOnly` 可瘦身成只含 sing-box。也可以先手动放进 `src-tauri/resources/`：

```bash
# macOS Apple Silicon / Intel
./scripts/fetch-bundled-core-darwin-arm64.sh    # 或 fetch-bundled-core-darwin-amd64.sh
./scripts/fetch-bundled-xray-darwin-arm64.sh    # Xray 内核 + geodata（另有 amd64 版）
./scripts/fetch-bundled-mihomo-darwin-arm64.sh  # mihomo 内核 + geodata（另有 amd64 版）
./scripts/fetch-bundled-rule-sets.sh
```

```bash
# Linux amd64
./scripts/fetch-bundled-core-linux-amd64.sh
./scripts/fetch-bundled-xray-linux-amd64.sh
./scripts/fetch-bundled-mihomo-linux-amd64.sh
./scripts/fetch-bundled-rule-sets.sh
```

```powershell
# Windows x64
pwsh scripts/fetch-bundled-core-windows-amd64.ps1
pwsh scripts/fetch-bundled-xray-windows-amd64.ps1
pwsh scripts/fetch-bundled-mihomo-windows-amd64.ps1
# 规则集由 build-windows.ps1 一并拉取
```

### macOS DMG

在 **macOS** 上执行（不必在本机对应架构上编：Apple 芯片可交叉编 Intel）：

```bash
# 按本机架构打包
./scripts/build-dmg.sh

# Apple Silicon
./scripts/build-dmg.sh --arch arm64

# Intel（x86_64）
./scripts/build-dmg.sh --arch intel
# 等价：
./scripts/build-dmg-intel.sh
```

脚本默认会把三颗内核（sing-box / Xray / mihomo）一并拉取打进安装包（`--singbox-only` 可瘦身只留 sing-box）。产物在：

`src-tauri/target/<aarch64|x86_64>-apple-darwin/release/bundle/dmg/`

### Windows 安装包

```powershell
pwsh scripts/build-windows.ps1                    # NSIS 安装包（默认，三内核）
pwsh scripts/build-windows.ps1 -Bundle msi        # MSI
pwsh scripts/build-windows.ps1 -Bundle portable   # 便携版：解压即用的 zip，数据存在 exe 旁
pwsh scripts/build-windows.ps1 -SingboxOnly       # 瘦身：只打包 sing-box
```

产物在 `src-tauri/target/release/bundle/nsis/` 或 `.../msi/`（便携版在 `.../portable/`）。未打包进安装包的内核，随时可在设置页在线下载。

### Linux AppImage

在 **Linux x64** 上执行。先装系统依赖（Ubuntu / Debian，其他发行版参见 [Tauri 前置要求](https://tauri.app/start/prerequisites/)）：

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

再打包（三内核 fetch 脚本见上文，缺失时需先执行）：

```bash
pnpm tauri build --config src-tauri/tauri.linux.conf.json
```

产物在 `src-tauri/target/release/bundle/appimage/`（`*.AppImage`）。

---

用着顺手的话，点一颗 [Star](https://github.com/zn0wii/satelite-proxy)，卫星会飞得更稳一点。

## 友情链接

- **佬友聚集地** [linux.do](https://linux.do/)

## Star History

<a href="https://www.star-history.com/?repos=zn0wii%2Fsatelite-proxy&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=zn0wii/satelite-proxy&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=zn0wii/satelite-proxy&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=zn0wii/satelite-proxy&type=date&legend=top-left" />
 </picture>
</a>
