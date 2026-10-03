# AGENTS.md — Satelite Proxy 项目地图

面向 AI agent 的项目速查文档。读完本文即可定位绝大多数代码，无需重复探索。
最后核对：2026-09-27（**内置 Cloudflare WARP（aether 副进程，免订阅出网）**：① 模型：`Protocol::Warp` + `ProtocolConfig::Warp`（无建模字段，编辑前后端双拦、解析器不可构造）；store 每次 load 幂等播种内置节点 `warp-builtin`（`BUILTIN_WARP_SUB_ID="builtin-warp"` 无 Subscription 行，`subscription_feeds_nodes`/`subscription_display_name` 为其开洞，enabled_nodes 绕过订阅过滤；server/port 仅作展示与 TCP ping 目标=162.159.198.2:443）。② `CoreKind::Aether`：**纯副进程 kind**（`parse` 不映射 "aether" 永不作主核；无 repo/下载，`check_config` 跳过预校验；`run_command_args`=identity toml + `--bind 127.0.0.1:18191`（`AETHER_SIDECAR_PORT` 固定端口，生成器 fallback 同值）+ `--scan turbo --quick-reconnect`；`spawn_env` 发 `AETHER_PROTOCOL=masque`（缺省 CLI 会进 stdin 交互菜单挂死）。③ 数据路径：三生成器都把 Warp 节点映射为 **loopback socks 出站**（sing-box builder 有 plan 走 plan、builder/xray/mihomo 无 plan 亦按常量端口发射），选择/池/规则/Clash 热切/delay 测速零改动；`compute_sidecar_plan` 把 Warp 委托提升到多核开关之前（与 protocol_cores 无关，custom 运行时除外；链跳内排除），**三主核模式（sing-box/Xray/mihomo）都会拉起 aether 边车**（Xray/mihomo 路径新增同款 plan→start_sidecar→失败整体回滚）；`prune_unavailable_sidecars` 对缺 aether 二进制只降级 WARP 节点（log warn、其余照常），不像 Xray/mihomo 委托那样硬失败。④ aether 二进制（2026-09-27 晚定稿：**上游预编译包**）——上游=CluvexStudio/Aether（MSN-GUARD 的 aether 即其 fork），`fetch-bundled-aether-{darwin-arm64,darwin-amd64,linux-amd64}.sh` + `fetch-bundled-aether-windows-amd64.ps1` 从上游 Releases 拉 **v2.1.0** 的 `aether-<os>-<arch>.tar.gz|.zip`（官方 .sha256 校验；包内 pt/lyrebird、psiphon-tunnel-core 为上游自带可选项，脚本只取主二进制），staged 到 `resources/bin/<平台目录>/aether(.exe)` + `aether-version.txt`（resolve_core_bin 通用布局自动识别，不入 git 同其它内核），四平台 resources 清单已接（singbox-* 瘦身单除外）；`build-dmg.sh`（双架构分支，intel 包装脚本自动继承）/`build-windows.ps1`（-SingboxOnly 分支外）/CI `release.yml`（matrix fetch-aether + 两个 fetch 步骤 + 两个 cache key hash）均已接入。历史上曾自编译 MSN-GUARD fork（AGPL-3.0，Windows 工具链=VS2026 MSVC + 用户级 NASM 2.16.03 + PyPI libclang wheel 抽 DLL，fork 源仅 socks_upstream.rs 需 cfg(unix) 门控 2 处）——上游包路线定稿后自编译仅作备用。⑤ 就绪语义：manager `wait_until_ready` 2s 判 FATAL 即标 Running（aether 首次 WARP 注册要 10-30s，不会卡启动）——`start_sidecar` 对 Aether 追加 ≤15s 端口 bind 等待（超时仅 info log「注册/网关选择较慢」，进程不退出），期间 WARP 节点 delay 测失败属正常；TUN 回环守卫 `sidecar_process_names` 已含 aether（进程规则直连放行）。⑥ 前端：节点列表/分组照常（组头=Cloudflare WARP 无 id 后缀，nodeTooltip subLabel 特判 builtin-warp）、详情弹窗有 warp 只读块、⋮ 编辑对 warp 隐藏、LogsPage 第五 tab WARP（get/clear_core_log_tail 新 "aether" kind）。功能开关=`settings.warp_enabled`（2026-09-27 晚追加：**默认关，独占模式**——设置→内核设置 顶部卡片开启；开启=**全部流量走 WARP**：`subscription_feeds_nodes` 对订阅节点返回 false（节点列表只剩 WARP、生成配置只含 warp 出站），`current_node_id` 随开关改指 warp-builtin/清空；关闭=恢复订阅节点、plan 不含 aether；切换走 `update_settings` 的 core_config_changed 防抖重启；**route_final 覆盖**：warp 开启时 `build_options()` 与 `generate/preview` 的两处 BuildOptions 字面量把 route_final 强制为 proxy（未匹配规则的流量兜底走 WARP——否则兜底直连会把探测源等无名域名悄悄放直连，网络探测卡因此显示真实 IP；用户显式直连规则如国内集仍优先）。）。启动坑（已修）：aether CLI 在 stdin 为 TTY 时进交互菜单（scan/IP/transport 三处 prompt，`--scan` 参数管不了 IP 版本）——manager 对 Aether 子进程 `stdin(Stdio::null())` + `spawn_env` 钉 `AETHER_SCAN=turbo`/`AETHER_IP=4`。**就绪等待不持锁（2026-09-27 晚）**：`start_proxy` 全程持有 runtime+store 双锁，最初放在 `start_sidecar` 里的 15s bind 等待把所有 store 命令堵死（全页面"加载中"、启动 15-30s）——已改为 spawn 后立即返回 + 分离线程纯端口探测记日志（≤60s，死亡由看门狗管），未 bind 前 WARP 节点就像普通未上线节点拨号失败。另：store 加载时的 `unknown variant \`warp\`` warn 来自同数据目录下的旧版打包实例（版本偏差，无害）。此前 2026-09-25（**内核支持详情面板 + mihomo 订阅原文透传（解析全收 + per-node raw）**：① 新命令 `get_subscription_core_support`（services/core_support.rs）：用已存原始报文按**当前解析器**重算——条目总数/识别数/三内核逐节点支持明细（`node_unsupported_reason` 单一真源给出原因，与 supports_node 过滤零漂移）/解析期跳过明细；订阅卡 ⋮ 新增「内核支持」弹窗（三条比例条 + 分内核不支持列表 + **一键复制分析报告**——版本/格式/逐节点原因，供真实用户反馈）。② **解析全收**：clash 解析器给每个成功条目附着 `ProxyNode.raw`（原始 YAML），mihomo 认识但应用未建模的类型（`MIHOMO_UNMODELED_TYPES`=ssr/mieru）由「跳过」改为**救援收纳**为 `Protocol::Unknown` + `ProtocolConfig::Unknown` 节点（id=raw 内容哈希，稳定）；其余未知类型照旧跳过（面板可见）。③ **mihomo 生成器原文透传**：带 raw 的节点在 `proxies:` 里原样输出（`mihomo_proxy_mapping`：name 重写为 `node-<id16>` tag 空间，组/规则/Clash API 热切全不变；provider 自带 `dialer-proxy` 按原名→tag 重写，引用缺失则整节点跳过并记日志——mihomo 会因找不到目标拒载整份配置）；`supports_node` 对 Mihomo+raw 放行（Naive/Tor/ShadowTls 除外=内核本身无此出站；实测 mihomo v1.19.30 `-t` 接受 xhttp/ss+shadow-tls 原文），sing-box/Xray 节点集合与此前完全一致（Unknown 在协议层被过滤、builder catch-all 兜底）；未知节点禁参数编辑（前后端双拦）；节点列表 wire 边界剥掉 raw 防载荷膨胀，见 §5.3/§5.4/§5.5/§5.6/§5.8/§6.3。此前（**订阅原始配置留存 + 卡片类型标签 + 查看配置弹窗 + 「自写配置」升级多核「自定义配置」**：① 订阅原始报文落盘 `subscriptions/<id>.txt`（`ImportOutcome.raw_body`，url/file/text/custom 每次导入/刷新更新；新命令 `get_subscription_raw_config`，node 档无正文，旧订阅下次刷新后才有）；② 订阅卡显示配置类型标签（`.sub-type-tag`：clash_yaml→mihomo、singbox_json→sing-box、uri/base64_uri→URI；custom 档显示 custom_kind）；③ 订阅/自定义卡 ⋮ 菜单新增「查看配置」弹窗（原始报文只读展示+复制）；④ `SubscriptionSource::Singbox` 泛化为 `Custom{content,kind}`（`CustomConfigKind` singbox/mihomo/xray；`detect_custom_config_kind` 判型=JSON 条目 `type`→sing-box / `protocol`→Xray、YAML Clash 顶级键→mihomo；serde 兼容旧 singbox wire），AddConfigModal 第四 tab 更名「自定义配置」并实时显示识别类型，订阅页第三分组更名「自定义配置」列全部自定义档+类型标签，`start_custom_proxy` 三分支按类型拉起对应内核（mihomo 写 `config/custom-<id>.yaml` 于 config/ 顶层使 `-d` home 推导正确 + external-controller 可用时接 Clash API；xray ensure geodata/wintun + 进程存活就绪；`custom_config_nodes` mihomo→parse_clash_yaml、xray→空列表；`RuntimeSource::Singbox` 枚举名与 wire `singbox:<id>` 保持不变、语义扩为任意类型），见 §5.2/§5.3/§5.6/§5.8/§6.3/§6.5。此前（**Windows 应用图标改由 mac 源图直接生成**：`icon.ico` 与 `Square*Logo/StoreLogo` 改从手调 macOS 源图 `assets/icon/ic_launcher-mac.png` **as-is** 生成（与 icns 同款处理：只缩放、不裁 bbox、不 96% 归一化、无辉光环——用户点名「用这张重做 Windows 应用图标、标题栏不动」，Windows 应用图标自此与 macOS 观感一致：瓦片 ~94%、手调圆角、透明角）；`tile_icon_1024`（96% 归一化 + 外辉光）管线**降为仅产 Linux PNG**（icon.png/32x32/128x128@2x，本次切换像素级不变）；ico ≤48px 锐化/gamma 调优与 20/28px 档保留；已知取舍=深色任务栏小尺寸下瓦片轮廓较弱（辉光环已去，若「显小」回潮可评估回加）；托盘/titlebar-* 未动；回退=`git checkout HEAD -- src-tauri/icons/icon.ico "src-tauri/icons/Square*" src-tauri/icons/StoreLogo.png`（源图未覆盖、无需归档目录），见 §1 图标生成条目。此前（**托盘唤醒窗口位置记忆**：托盘双击重建窗口（低内存模式下关窗销毁 WebView）此前从不设置位置，Windows 对新窗口 CW_USEDEFAULT 级联摆放导致每次唤醒位置漂移——修复=`window_size_<mode>` 文件扩为 `"<w> <h> <x> <y>"`（旧两段式向后兼容），`persist/restore_main_window_layout`（原 *_size 改名）在隐藏/退出时连位置一起持久化、托盘重建 `show_main` 与冷启动均恢复到记住的位置，`position_reachable` 对各显示器工作区做可达性校验（拔显示器/改分辨率后落屏外→居中兜底），无记住位置亦居中；单测覆盖新文件格式解析，见 §9.6。此前（**托盘 saturn 组换为 frost 版、消除与 saturn_classic 重复**：实测 saturn-on/off 与 saturn-classic-on/off 哈希相同（两组同图），用户要求用新图标重做托盘——重跑 `generate-tray-icons.py`（`draw_saturn_tile` 本就从应用图标源图生成，saturn 组自动变 frost；saturn-classic-* 手工归档不被脚本覆盖、保持 facenew 笑脸土星；4 个手工托盘图标已按惯例恢复）；**托盘 saturn 组自此恢复随应用图标联动**（2026-09-23「不联动」决定被本请求取代），**titlebar-* 标题栏图标仍不动**（badge 式）；托盘 PNG 经 include_bytes 嵌入、重编生效，见 §1/§5.7。此前 2026-09-24（**任务栏图标观感二次修复：瓦片归一化 ~96% + 冰蓝外辉光**：96% 归一化上线后用户仍觉任务栏显小——实测 exe 已嵌新图标、瓦片 bbox 已撑满槽位，根因是**观感**而非边距：瓦片近黑（RGB ~[20,22,23]）、任务栏深色，可辨认的只有内部行星发光体（亮像素仅 ~24%），邻居满方形彩色图标相形之下显大；修复=`tile_icon_1024` 在 96% 瓦片下叠加**柔和外辉光**（`TILE_HALO_WIDTH=3%` 画布宽、GaussianBlur(宽/2.5)−腐蚀 alpha 的环形、强度 1.0，色值 `_glow_tint` 从画作最亮像素动态采样=冰蓝白 ~[244,251,255]，纯 Pillow 无 numpy 依赖），深浅背景目检均成立（3% 足够/5% 略糊已弃、96px 不廉价）；仍非 2026-09-22 被否的垫底满幅方案——辉光是描轮廓不是填底；**Windows 任务栏图标缓存粘滞**：重编后若仍显旧图标需刷新图标缓存（任务栏取消固定→重新固定，或 ie4uinit -show / 重启 explorer）；见 §1 图标生成条目。此前 2026-09-24（**Windows/Linux 图标产物瓦片归一化 ~96%**：`generate-app-icons.py` 新增 `tile_icon_1024`——生成期把源图瓦片 bbox 放大到画布 96%（`TILE_ART_SCALE`，瓦片自身圆角与透明角保留、无垫底无额外遮罩），`make_app_icon` 与 `_ico_entry` 均改从它重采样，修复 86% 透明边距使 Win11 任务栏图标显小；icns 满幅管线走独立 bbox 归一化不受影响（重跑哈希不变）；**这不是** 2026-09-22 被否的 mac 式满幅变体（深色垫底+15% 遮罩与瓦片 ~25% 圆角不重合、角部露垫底）——那条禁令针对「垫底+额外遮罩」方案仍然有效，本方案只放大瓦片本体；源图保持 86% 边距惯例，icon-frost 归档产物同步刷新，见 §1 图标生成条目。此前 2026-09-23（**应用图标换代 icon-frost**：全套应用图标换为「冰蓝发光笑脸土星」新稿——源稿 `src-tauri/icons/new.png`（1254px 纯黑底 RGB）经鲁棒圆角矩形拟合提取（同 icon-neon 配方且更简单：本次辉光无瓦片外溢，直边 L122/R1132/T122/B1121、共享角半径 ~274，配方见 `icon-frost/README.md`）→ 876px @86% 居中源图，**仅重跑 `generate-app-icons.py`**（现配方 ico 20/28px 档、≤48px 锐化/gamma 提亮、icns 满幅变体，均目检复核）；**托盘与 Windows 标题栏图标未联动**（用户明确要求不动：tray/saturn 组与 titlebar-* 保持土星环版，恰与上一代相同、无观感断裂）；新套按惯例归档 `icon-frost/`（含 1254px 黑底原始稿，归档由四处变五处）；win0-2 候选稿随之删除、`generate-windows-app-icon.py` 脚本保留，见 §1 图标生成条目。此前 2026-09-23（**应用图标回退 icon-saturn**：全套应用图标自 `icon-saturn/` 归档回退为土星环版（v1.0.38 稿）——源图与产物拷回后重跑 `generate-app-icons.py` + `generate-tray-icons.py` 套用现配方（ico 20/28px 档与 ≤48px 调优保留、icns 走满幅变体，均目检复核；托盘 saturn 组随源图联动为土星环版，4 个手工托盘图标已按惯例从 HEAD 恢复）；霓虹稿按换代惯例归档新设 `icon-neon/`（黑底提取配方迁至其 README），归档由三处变四处；Windows 专属候选管线 `generate-windows-app-icon.py` + win0-2 保留但非当前生效（其 win0 产物已被覆盖），见 §1 图标生成条目。此前 2026-09-23（**TLS 分片 + 设置页「内核设置」tab**：设置 multiCore tab 改名「内核设置」，顶部新增「TLS 分片」双开关（sing-box/Xray 各一，仅对应内核模式生效，mihomo 内核无此能力故无开关，默认全关）。sing-box 侧 `BuildOptions.tls_fragment_singbox` → 全部 `tls.enabled` 节点出站打 `tls.fragment:true`（v1.13.18 该开关在出站 `tls` 对象内、布尔型，真机 check 已验证，live 测试 `live_tls_fragment_config_validates`）；Xray 侧 `tls_fragment_xray` → freedom `fragment-out`（tlshello/100-200/10-20ms）+ tls/reality 节点出站 `sockopt.dialerProxy`，副进程配置带 `tls_fragment` 参数随行（委托节点真实拨号在副进程）；开关变更走 `update_settings` 的 `core_config_changed` 防抖重启，见 §5.3/§5.4/§6.3）。此前 2026-09-23（**订阅 id 显示收敛为「悬停/分组」两处**：`Subscription::display_name()` 删除，`to_view().name`、`ListedNode.subscription_name`、`runtime_profile_name`、连接日志 `node_tag_info_map` 一律下发干净订阅名；内联 `名字(id:末4位)` 格式统一走 `nodeTooltip.ts::subLabel`——节点悬停卡、NodeDetailModal 订阅行、按订阅分组组头标签、SimpleConnect 停止态列表悬停；**节点行/卡全卡只有一个悬停气泡**（`nodeHoverTitle`=节点名/订阅`名字(id:末4位)`/完整订阅 id `id: <hash>` 行/endpoint/TLS 标记/UDP）——卡内子元素（`.node-sub-label`/`.node-proto-tags`/`.node-card-name`/latency 标签）一律不带自身 `title`，悬停落到行/卡根级气泡（fastTooltip 的 closest 冒泡），收藏心形等图标按钮的控件级提示除外；完整 id 悬停处：ConfigPage 订阅卡 `<h3 title=id>`、Dashboard/SimpleConnect 自定义配置标题 `title=runtime_profile_id`；按订阅分组改按 `subscription_id` 分组（`nodeGroups.ts`，同名同 URL 多订阅不合并组），见 §5.3/§5.8/§6.3/§6.5）。此前 2026-09-23（**多核模式 TUN 回环修复**：TUN + 副进程委托下，副进程自身出站（拨真实节点服务器，普通 socket）被主核 TUN auto_route 抓回、按 final=proxy 路由回副进程自己——无限回环、无法上网；sing-box 自身出站靠 `route.auto_detect_interface` 的**进程级**绕过不受影响，但该保护不覆盖 xray/mihomo 副进程。修复=`build_singbox_config` 在 sniff 后、hijack-dns/block_quic/用户规则前注入 `{process_name:[xray,xray.exe,mihomo,mihomo.exe]→direct}` 守卫（tun_enabled 且 plan 非空才发；sing-box 按 `filepath.Base(exe路径)` **大小写敏感精确匹配**故双平台变体都发；配置含 process 规则即自动启用进程查找、不依赖 `find_process` 设置；置于 hijack-dns 前兼防副进程 DNS 循环依赖「经远程 DoH 解析节点域名→DoH 走代理→代理需要该域名」、置于 block_quic 前防误杀 mihomo 副进程的 hy2/tuic QUIC 出站）+ IP-literal 服务器委托节点另发 `ip_cidr→direct` 兜底（进程查找失效时防回环，如未来 macOS sysctl ABI 变动；域名型/CDN 服务器只能靠 process 规则），见 §9.20⑥）。此前 2026-09-23（**节点三件套 + 同 URL 多订阅**：① 节点卡片附属信息——`nodeTooltip.ts::nodeFeatureBadges`（reality/tls/非 tcp 传输）在 NodesPage 行/卡与 SimpleServers 行协议 tag 旁渲染小徽标（`.node-proto-feat`），分组/搜索仍按主协议；② 单节点参数编辑——新命令 `get_node_draft`/`update_node`（commands/config.rs + store.rs `update_node_from_draft`：`node_to_draft`/`draft_to_node` 往返、保留订阅归属与 source、Xhttp mode/extra 回填防丢、id 轮换时 `remap_node_refs` 随行迁移首选/规则钉定/链路/池/收藏、身份未变保留延迟、enabled 节点集变化走防抖重启），前端 `EditNodeModal`（NodeDraftFields 复用 + 「订阅更新会覆盖编辑」警告），⋮ 菜单「编辑节点」，custom 运行时不显示；③ **同 URL 允许多次订阅**——订阅 id 唯一化（`unique_subscription_id`：源哈希为首选、撞库加熵重哈希，`add_subscription_url/text/singbox/node` 均传新鲜 id；`update_subscription` 去除同 URL 合并删除逻辑、恒保自身 id），**节点 id 改为订阅作用域哈希**（`ProxyNode::scoped_id`/`with_scoped_id`，`build_outcome` 统一走它——同订阅刷新 id 稳定、跨订阅同后端不撞 id），store 迁移 **v11**（`migrate_subscription_scoped_node_ids`：重算全部节点 id + `remap_node_refs` 重映射 current_node_id/rule_sets 内外 node_id(s)/pools/chains/favorites，写 `store.pre-v11.backup.json`）；订阅显示名后缀 `Subscription::display_name()`=`名称(id:末4位hex)`（`to_view` 与 ListedNode.subscription_name、runtime `node_tag_info_map`/`runtime_profile_name` 均走它，`to_detail` 保持干净原名供编辑回填）；AddConfigModal 重复 URL 由警告改信息提示（`modal.duplicateUrlInfo`，允许提交）。**不变式**：编辑是临时的（订阅刷新整组重建即覆盖，警告已示）、URL 轮换走「编辑订阅」保 id（节点 id 随之稳定）、`node_aliases` 键仍不含订阅 id（跨订阅孪生共享改名，可接受）、custom singbox 节点不可编辑且 id 仍无作用域，见 §5.2/§5.3/§5.8/§6.5）。此前 2026-09-23（**Windows 标题栏小图标修复 + 土星经典托盘样式**：① `window_icon.rs` 新模块——Tauri 默认窗口图标是从 icon.ico 抽的单帧 RGBA 经 tao `CreateIcon` 成一个大尺寸 HICON，标题栏 GDI 硬缩必糊；改为 SMALL/SMALL2 用内嵌 `icons/tray/titlebar-<16..48>.png`（badge 薄荷绿卫星标记，用户指定的标题栏图样，`draw_tray_badge` 按尺寸原生重绘）`CreateIcon` 原生尺寸零缩放，BIG（任务栏/Alt-Tab）走 exe 图标资源组（id 32512）按 DPI `LoadImage` 保持土星应用图标；挂点=setup/show_main 重建/Focused/ScaleFactorChanged，(hwnd,dpi) 记忆去重，见 §5.7。② 旧 facenew saturn 托盘图标恢复为 `saturn_classic` 样式（手工归档 PNG）与霓虹新版并存。③ icon.ico 补 20/28px 档+≤48px 单步直降+RGB 锐化/gamma 提亮，见 §1 图标生成条目）。另 2026-09-23：**Windows 托盘单击修复**——Tauri 2 tray 默认 `show_menu_on_left_click=true`（左键弹菜单），`tray.rs` 按平台设 `!cfg!(windows)`：Windows 左键=唤起主界面（既有 on_tray_icon_event）、右键=菜单；macOS 保持原生左键弹菜单惯例。此前 2026-09-22（**应用图标更换为「霓虹笑脸土星」**：ChatGPT 生成稿（1254px 纯黑底）→ 鲁棒圆角矩形拟合抠除黑底与瓦片外辉光 → 876px @86% 居中源图；全套产物重生成，托盘 saturn 组随源图联动更新；旧「笑脸土星」图标归档 `icon-facenew/`（v1.0.40）。Windows 产物曾随批改满幅后又按用户决策回退为透明边距（缘由见 §1 图标生成条目），配方与归档清单见 §1 图标生成条目）。此前 2026-09-21（**UI 英文适配清零（前端）+ 托盘双语**：前端全部硬编码中文接入 i18n——`AddConfigModal`（`modal.*`）、`NodeDraftFields` 手动节点表单（`nodeDraft.*`）、⋯ 快捷菜单（`uiMenu.*`）、ErrorBoundary/ThemeSwitch/TopNav/NodesPage 等散点；托盘菜单文案按 `settings.locale` 双语（`tray.rs` 的 `TRAY_LABELS_ZH/EN` + `refresh_labels`，UI 切语言即时跟随）；订阅到期中文习语（长期有效等）en 下显示期映射（`ConfigPage::localizeExpireText`）；内置集名「系统 Hosts」由 HostsPage 按 id 覆盖显示。后端错误消息仍为中文（存量，见 §6.6 已知残留），见 §5.7/§6.6）。此前 2026-09-16（**探测数据共享管线**：`AppStore::update_node_latency` 成为全部延迟写入的唯一入口，带真实延迟优先规则（`clash_api` 恒覆盖、`tcp` ping 不得覆盖真实值——含失败的真实值，口径存 `StoredNode.latency_method`，随 `ListedNode` 平铺下发）；手动批量测速与智能引擎（巡检/扫描/规则池探测）统一经 `AppState::apply_latency_results`/`try_apply_latency_results` 落库，被接受的结果经 `node-latency-changed` 事件（`state.rs` 全局 AppHandle 注册，`api.ts::onNodeLatencyChanged`）推送前端，Dashboard 延迟卡（含 `running:nodeId` 边沿自动补测延迟）/NodesPage/SimpleServers 行内实时刷新（tested_at 防旧回滚），见 §5.2/§5.8/§6.2/§7。此前 2026-09-13：**Xray 传输面修正 + 真内核矩阵测试**：① h2/http 传输已被 Xray v26 配置层移除（`network:"http"` 拒载）——Xray 生成期报错跳过 + `supports_node` 列表过滤，sing-box/mihomo 原生可用；② xhttp `extra` 调优参数接入：分享链 base64url `extra=`（裸 JSON 亦收）与 clash `xhttp-opts.extra` 解析入 `Transport::Xhttp.extra`，原样嵌入 `xhttpSettings.extra`，非法 JSON 丢弃防整配置拒载（v2rayN 对齐，edgetunnel xhttp 场景）；③ 新增真内核 live 验证 `live_share_link_pipeline_validates`（真实分享链接全管线）与 `live_all_protocol_matrix_validates`（全协议×传输×TLS 矩阵 + Valid/GenReject/CoreReject 预期表），见 §8。**智能切换二次重构**：`smart_switch.rs` 新增出口巡检（每 tick 经内核 URL 探测当前出口、快慢双档复核防抖，死出口不依赖用户流量即可发现）+ 恢复扫描（死出口绕过 dwell/cooldown：ping 分批排序 → top3 经内核 delay API 实测验证 → 取最低验证延迟者热切；ping/verify 失败递增弹出 30s→30min），URL 延迟为唯一对比口径、绝不凭 ping 上位，见 §5.1。此前 2026-09-12：**MASQUE 接入 + 三核多副进程**：新增 `Protocol::Masque`（mihomo 独占，usque 式 ECDSA 密钥对；mihomo 主内核原生可用），副进程机制从单一 Xray 泛化为 **Xray + mihomo 双副进程**（`SidecarPlan` 条目带 kind、单一连续端口空间、看门狗按副进程独立跟踪），masque 节点在 sing-box 主核下钉选到 mihomo 副进程经 loopback socks 委托，见 §5.4/§5.6/§9.20。窗口两模式开放自由放大：pro ≥960×720 / simple ≥320×480 无上限，经 `useViewportScale` 等比缩放，尺寸按模式持久化，见 §4/§9.6。此前 v1.0.9，三内核：sing-box / Xray / mihomo；新增首页「网络探测」卡=延迟+出口 IP 竞速探测，见 §5.6/§5.8/§6.3。内核意外退出修复：watchdog 真重启 + `core-status-changed` 事件 + 启动就绪须实测 mixed 端口拨号，见 §5.1/§5.6/§6.2。智能切换被动检测 2026-09 重构：失败判定=≤3s 快死或 ≤15s 零字节（拨号超时带），mihomo 增设内核日志流 `/logs` WS 监听（§5.1 `log_listener.rs`）；同批性能改造=快照流三档变速（需求心跳+TUN 退避）与 passive stats 单趟化，见 §5.1。CI 与本地打包脚本默认改为三内核四平台全打包，Linux 补齐 xray/mihomo 基础设施，mihomo geodata 改为固定快照机制，sing-box 版本号三处统一为 v1.13.18，见 §9.19；mihomo 的 amd64 发行包改用 `-compatible`（GOAMD64=v1）变体，修复 Intel 版应用在 Rosetta 2 / 老 Intel CPU 上安装 mihomo 即 fatal 的问题，见 §9.17⑦。远程加密 DNS 开放用户自定义：`DnsSettings.remote_dns` 非空即整体替换内置池，三生成器+诊断统一走 `effective_remote_pool()`，见 §18①。规则集「指定」支持多选节点池：`RuleSet.node_ids`（2+ 勾选=整组显式池，与 Filter 池同 tag `smart-<集id>`、同生成/维护路径，勾 1 个仍走存量单钉 `node_id`），见 §5.3/§5.4/§5.5）。

## 0. 阅读与维护规则（必读）

**对 agent 的要求**：在本仓库做任何改动前，先通读本文档——尤其是「§1 快速上手」、§7 修改场景速查表、§9 约定与坑。不要凭猜测探索全库。

**文档同步规则**：项目发生重大变动时，**必须与代码同一次提交同步更新本文档**，包括但不限于：

| 变动类型 | 需更新的章节 |
|---|---|
| 新增 / 删除 / 移动模块或源文件 | §3 目录速览、§5/§6 对应模块详解 |
| 新增 / 改名 command 或 Tauri 事件 | §5.1（注册表）、§5.8、§6.2 |
| 数据模型 / 存储结构 / 磁盘布局变化 | §5.2、§5.3 |
| 配置生成或内核管理方式变化 | §5.4、§5.5 |
| 构建 / 打包 / 测试流程变化 | §1 快速上手、§8 |
| 新的平台分支、新的坑 | §9 |

小改动（文案、bugfix、样式微调）不强制更新；文中行数标注允许过时，以「文件存在性与职责描述」为准，发现明显过时顺手修正并更新文首「最后核对」日期。

### 更新说明输出模板（用户要求的固定格式）

用户让「生成更新说明 / 改动摘要」时，**必须按以下模板输出**（更新说明 4–5 条精炼要点 + 固定的下载指南与 macOS 隔离提示）：

````markdown
### 更新说明

- **<要点>**：<一句话说明>
- …（4–5 条，重要功能加粗开头）

### 下载指南

* **macOS Apple Silicon（M 系列）** → `aarch64.dmg`
* **macOS Intel** → `x64.dmg`
* **Windows 64 位** → `x64-setup.exe`
* **Linux 64 位** → `amd64.AppImage`

根据你的系统和 CPU 架构选择对应安装包即可。


**macOS 如果提示「Satelite 已损坏，无法打开」**，可在终端执行：

```bash
sudo xattr -d com.apple.quarantine /Applications/Satelite.app
```
````

## 1. 快速上手：环境 · 编译 · 测试 · 打包

### 环境要求

- **Node + pnpm**（registry 已锁定 npmmirror，见 `.npmrc`；依赖只能用 pnpm 装）
- **Rust stable**：Windows 需 MSVC 工具链（build 脚本用 vswhere 检测）；macOS 需 Xcode CLT
- 平台限制：DMG 只能在 macOS 打，Windows 安装包只能在 Windows 打；Apple Silicon 可交叉编 Intel（脚本自动 `rustup target add x86_64-apple-darwin`）

### 开发调试

```bash
pnpm install        # 安装前端依赖
pnpm tauri dev      # 一键启动 Rust 后端 + WebView（Vite 端口 1420 strictPort）
```

- 前端改动走 HMR；Rust 改动自动重编并重启应用
- **不要用 `pnpm dev` 调 UI**——只起 Vite 没有后端，所有 `invoke` 会失败；UI 调试也用 `pnpm tauri dev`
- 首次 dev 缺 sing-box 内核 / 内置规则集会**自动联网下载**；离线环境先跑下面的「资源预取」
- 深链调试： schemes 为 `clash://` `sing-box://` `singbox://`（Windows/Linux dev 下启动时自动注册）

### 检查与测试

```bash
pnpm build                                        # 前端：tsc 严格类型检查 + vite 产物（提交前必过）
cd src-tauri && cargo check                       # Rust 快速检查
cd src-tauri && cargo test                        # Rust 全部测试（含散落 #[cfg(test)] 单测）
cd src-tauri && cargo test --test parse_subscription          # 只跑订阅解析集成测试
cd src-tauri && cargo test --test download_core_live -- --ignored  # 真网下载 live 测试（慢，慎跑）
cd src-tauri && cargo fmt / cargo clippy          # 标准 Rust 工具链
```

- 前端**没有** ESLint/Prettier 配置，质量门槛 = `tsc` strict + `pnpm build`
- 测试 fixtures 在 `src-tauri/tests/fixtures/`（clash yaml ×2、singbox json ×1）

### 打包发布

```bash
# macOS DMG（产物: src-tauri/target/<aarch64|x86_64>-apple-darwin/release/bundle/dmg/）
./scripts/build-dmg.sh                        # 按本机架构，三内核（默认）
./scripts/build-dmg.sh --arch arm64
./scripts/build-dmg.sh --arch intel           # 交叉编译；等价 build-dmg-intel.sh
./scripts/build-dmg.sh --singbox-only         # 瘦身：只打 sing-box（缺失自动 fetch）

# Windows（产物: src-tauri/target/release/bundle/nsis/ 或 .../msi/）
pwsh scripts/build-windows.ps1                        # NSIS 安装包，三内核（默认）
pwsh scripts/build-windows.ps1 -Bundle msi            # MSI
pwsh scripts/build-windows.ps1 -SingboxOnly           # 瘦身：只打 sing-box（缺失自动 fetch）

# Windows 便携版（产物: src-tauri/target/release/bundle/portable/Satelite_<版本>_x64_portable.zip）
pwsh scripts/build-windows.ps1 -Bundle portable       # 解压即用 zip：exe + resources/ + portable.flag（见 §9.19），三内核
pwsh scripts/build-windows.ps1 -Bundle portable -SingboxOnly  # 单内核便携版

CI（`.github/workflows/release.yml`）只打三内核版（sing-box + Xray + mihomo，含各自 geodata；`tauri.conf.json` 的 bundle targets 已收窄为 app/dmg/nsis/appimage——不再产 MSI/deb/rpm，本地脚本传 `--bundles` 仍可覆盖），
四平台一致，产物名/应用名/安装路径均为原版 `Satelite`（**无 -fullcores 等变体后缀，不要再给
productName 加变体名**——它同时决定 Windows 安装目录与 WebView2 数据目录位置）；
`tauri.<平台>.conf.json`（macOS arm64 走基础 `tauri.conf.json`，intel 走
`tauri.macos-intel.conf.json`）为全内核 overlay，只覆盖 resources 清单。单内核瘦身版
**仅本地脚本**：`tauri.singbox-<平台>.conf.json` overlay 瘦身 resources 只含 sing-box，
供 `--singbox-only`/`-SingboxOnly` 选用（体积敏感场景，产物同样叫 Satelite）。
三入口切内核 UI 不受影响——未打包的内核可经设置页下载。
```

CI 的三内核+规则集下载挂 `actions/cache`（path=`resources/bin/<平台>`+`resources/rule-sets`，key=对应平台 fetch 脚本内容 hash——升版本自动失效，命中即零外网下载），fetch 步骤统一经 `scripts/ci-fetch-with-retry.sh` 执行（3 次尝试/间隔 30s 退避，应对托管 runner 偶发连不上 github.com 的网络抖动；.ps1 自动用 pwsh 跑）；`windows-portable` job 用与 release Windows 条目相同的 key 复用缓存并预取。

打包脚本会自动拉取对应平台的官方内核并打进安装包，无需手动准备（mihomo geodata 例外，见下）。

### 资源预取（可选，离线/加速用）

```bash
scripts/fetch-bundled-core-darwin-arm64.sh        # macOS arm64 sing-box（默认 v1.13.18）
scripts/fetch-bundled-core-darwin-amd64.sh        # macOS Intel
scripts/fetch-bundled-core-linux-amd64.sh         # Linux amd64 sing-box
pwsh scripts/fetch-bundled-core-windows-amd64.ps1 # Windows sing-box v1.13.18 + libcronet.dll，支持 -Proxy
scripts/fetch-bundled-xray-darwin-arm64.sh        # macOS arm64 Xray（默认 v26.3.27）+ geosite/geoip.dat
scripts/fetch-bundled-xray-darwin-amd64.sh        # macOS Intel Xray
scripts/fetch-bundled-xray-linux-amd64.sh         # Linux amd64 Xray
pwsh scripts/fetch-bundled-xray-windows-amd64.ps1 # Windows Xray + geodata + wintun.dll（TUN 用），支持 -Proxy
scripts/fetch-bundled-mihomo-darwin-arm64.sh       # macOS arm64 mihomo（默认 v1.19.30）+ mihomo-geodata/（mmdb+GeoSite.dat）
scripts/fetch-bundled-mihomo-darwin-amd64.sh       # macOS Intel mihomo
scripts/fetch-bundled-mihomo-linux-amd64.sh        # Linux amd64 mihomo
pwsh scripts/fetch-bundled-mihomo-windows-amd64.ps1 # Windows mihomo + mihomo-geodata/（wintun.dll 与 Xray 共用），支持 -Proxy
scripts/fetch-bundled-mihomo-geodata.sh           # 一次性刷新 mihomo geodata 固定快照（见下）
scripts/fetch-bundled-aether-darwin-arm64.sh      # macOS arm64 aether（上游 CluvexStudio/Aether 预编译包）
scripts/fetch-bundled-aether-darwin-amd64.sh      # macOS Intel aether
scripts/fetch-bundled-aether-linux-amd64.sh       # Linux amd64 aether
pwsh scripts/fetch-bundled-aether-windows-amd64.ps1 # Windows aether v2.1.0，支持 -Proxy
scripts/fetch-bundled-rule-sets.sh                # 3 条内置 .srs 规则集（校验 SRS 魔数，--force 重下）
scripts/memory-profile/                           # WebView2 内存剖析（CDP 堆采样 + 进程树 RSS，见其 README 与 docs/webview2-memory-optimization-plan.md）
```

- 各平台 `fetch-bundled-mihomo-*` 脚本的 geodata（Country.mmdb + GeoSite.dat）**不联网下载**，而是从
  `src-tauri/resources/geodata/mihomo/`（git 跟踪的固定快照）复制。上游 MetaCubeX/meta-rules-dat 只维护
  滚动的 `latest` release、没有版本化 tag，无法像内核二进制一样 pin URL；刷新快照需手动执行
  `scripts/fetch-bundled-mihomo-geodata.sh` 后 `git add` 提交。

- 全部 fetch 脚本**幂等且版本感知**：目标二进制已 staged 且 `*-version.txt` 与脚本内置版本一致即跳过（升版本重跑自动刷新，不会停留在旧内核）；`build-dmg.sh` / `build-windows.ps1` 无条件调用它们——跳过判断的单一真源在 fetch 脚本内，外层不再做「文件存在就不调」的门控（旧门控会在升版本后静默打包旧内核）。CI 批量入口为 `scripts/ci-fetch-with-retry.sh`。

- 这些二进制**不入 git**（`.gitignore` 排除 `resources/bin/**/sing-box*`、`xray*`、`mihomo*`、`*.dat`、`wintun.dll`、`mihomo-geodata/`、`libcronet.*`、`resources/rule-sets/*.srs`），本地缺失属正常；
  唯独 `src-tauri/resources/geodata/mihomo/`（country.mmdb + geosite.dat 快照）**入 git**，不在排除列表内
- 图标生成（三个脚本，均依赖 Pillow，互不触碰对方产物）：`scripts/generate-app-icons.py` 只做应用图标——重采样源图 `assets/icon/ic_launcher-web.png`（**2026-09-23 换代为「冰蓝发光笑脸土星」= icon-frost**：源稿=用户提供的 1254px 纯黑底 RGB（原 `src-tauri/icons/new.png`，存档 `icon-frost/original-1254-blackbg.png`），经鲁棒圆角矩形拟合提取——比霓虹稿简单：辉光无瓦片外溢，直边 L122/R1132/T122/B1121、共享角半径 ~274，配方见 `icon-frost/README.md`；此前的土星环版（ec487bd/v1.0.38 稿）已归档 `icon-saturn/`，霓虹稿的 1254px 纯黑底提取配方在 `icon-neon/README.md`）。换新源图时按「圆角已内建在源图 alpha、内容占画布 ~86% 居中」格式对齐；该目录其余文件为 Interstellar 安卓素材、.gitignore 排除）出 icon.png/.ico/.icns/Square* 全套。**ico 小尺寸条目专项调优（2026-09-23）**：`write_ico` 的 ≤48px 条目单步 LANCZOS 直降 + 仅 RGB 走 unsharp（≤32px 1.4/100 再叠加 gamma 0.88 中间调提亮，48px 1.2/80，alpha 不动保圆角 AA）——按暗调霓虹稿标定（暗瓦片+暗星球+软辉光，纯重采样在 16–24px 糊成一团）；土星环回退后沿用、icon-frost 换代继续沿用并目检复核（更亮稿上表现为对比更利落、16–24px 土星轮廓可辨，无过曝/振铃）。且 ico 必须含 **20/28px 档**（125%/175% DPI 点名要这两档，缺了 Windows 会拉伸邻近档显示、更显糊——`icon-saturn/` 归档的旧 ico 即只有 16/24/32/48/64/128/256 七档）。历史图标归档五处、回退=产物拷回 `src-tauri/icons/`、源图拷回 `assets/icon/` 覆盖后重跑两生成脚本（**2026-09-23 frost 换代时托盘一度不联动，2026-09-24 起恢复**——用户要求用 frost 重做 saturn 托盘组以消除与 saturn_classic 的同图重复，故换稿/回退需连带重跑托盘脚本，跑完务必按文末「坑」恢复 4 个手工图标；titlebar-* 标题栏图标保持 badge 式、不随应用图标联动）：`icon-frost/`（冰蓝发光笑脸土星版，2026-09-23，**当前生效**，含 1254px 黑底原始稿）、`icon-neon/`（霓虹笑脸土星版，2026-09-22）、`icon-facenew/`（笑脸土星版，v1.0.40）、`icon-saturn/`（土星环版，v1.0.38）、`icon-legacy/`（v1.0.37 及之前旧版）。**icns 走手调源图 `make_mac_icon`**（2726961：payload 直接取自手工调校的 `assets/icon/ic_launcher-mac.png`，只缩放、不裁剪/不归一化/不垫底/不套遮罩——瓦片 ~94%、圆角手调，同时照顾 macOS ≤15（icns 位图原样显示）与 26 Tahoe（系统 squircle 重遮罩 + 给透明边距垫浅色底板）；更早的程序化满幅推导「放大 96% + 垫不透明深底 + 自身 15% 圆角」即被其取代）。**Windows 产物（ico/Square*/StoreLogo）自 2026-09-24 晚起改走 mac 源图 as-is**（用户点名「用 ic_launcher-mac.png 重做 Windows 应用图标、标题栏不动」：`_ico_entry` 基底与 Square*/Store 改用 `mac_icon_1024`/`make_mac_icon`——瓦片 ~94%、手调圆角、无辉光环，与 macOS 观感一致；≤48px 锐化/gamma 与 20/28px 档保留；已知取舍=深色任务栏小尺寸下瓦片轮廓较弱、主要靠行星发光体辨认）。**瓦片归一化 ~96% + 柔和外辉光管线（`tile_icon_1024`，2026-09-24 上午上线修复任务栏显小）现仅产 Linux PNG**（icon.png/32x32/128x128@2x：源图瓦片 bbox 放大到 `TILE_ART_SCALE=0.96`、保留自身圆角与透明角，沿轮廓叠 `TILE_HALO_WIDTH=3%` 辉光环——blur−腐蚀 alpha 环形、色取 `_glow_tint` 画作最亮像素均值=冰蓝白）；源图本身保持 86% 边距惯例，本次切换 Linux PNG 像素级不变。**Windows 任务栏图标缓存粘滞**：重编后旧图标可能滞留，需取消固定重固定或刷新图标缓存。**mac 式满幅变体（深色垫底 + 自身 15% 遮罩）在 Windows 上试过并被否（2026-09-22 用户决策）**：源图瓦片自带 ~25% 大圆角，与满幅自身 15% 圆角不重合、角部露出深色垫底、观感「被切成方的」——勿再给 Windows/Linux 产物垫底或套额外遮罩，放大瓦片本体即可。icns 为纯 Python 写入、无需 macOS iconutil，Windows 上也能全量再生成；`scripts/generate-windows-app-icon.py` 只做 **Windows 平台应用图标**（2026-09-23，用户指定 win0 稿）：仅产 `icon.ico`（exe/安装包/任务栏，经 tauri-build 嵌入资源 id 32512）+ `Square*Logo/StoreLogo`（msi 本地打包用），**不写** icon.png/icns/32x32/128x128 等跨平台产物、也**不碰 tray/**（标题栏图标 titlebar-* 归托盘脚本与 `window_icon.rs`，与本脚本无关）——源图默认 `src-tauri/icons/win0.png`（512px RGBA 霓虹笑脸土星候选稿，圆角瓦片透明边距已内建，但自带**向右下偏移的纯黑软投影** alpha≤66），管线=按 alpha≥200 取瓦片硬边 bbox **裁掉黑影**（浅色壁纸/任务栏上成黑晕、16–24px 糊边）+1px AA 余量 → 归方、原生分辨率居中 86% → ico 配方与主脚本 2026-09-23 调优同款（20/28px 档、≤48px 单步 LANCZOS + RGB unsharp、≤32px 叠 gamma 0.88）；换源图传 argv[1]（win0-2 候选稿已于 2026-09-23 晚 icon-frost 换代时删除——脚本保留、源图需自备，1254px 黑底 RGB 稿先按 `icon-neon/README.md` 配方提取）；**非当前生效管线**（已被主脚本 2026-09-24 的 mac-as-is 管线进一步取代——运行它会把 ico 打回 86% 旧观感，勿再跑）。`scripts/generate-tray-icons.py` 只做托盘图标（badge/saturn/mark/ghost/buddy 脚本生成；danger*/ghost2/faceid/**saturn_classic** 为手工产物不在脚本内——saturn_classic=霓虹化之前的 facenew 土星（v1.0.40，自 git 历史恢复），2026-09-23 应用户要求与新版并存；saturn 组=应用图标同款土星瓦片（**2026-09-24 起重新随应用图标联动=icon-frost 版**，用户要求用新图标重做托盘消除与 saturn_classic 的重复；off 去色/on 原色）；脚本另产 `tray/titlebar-<16..48>.png` 各尺寸 badge——`window_icon.rs` 内嵌作 Windows 标题栏图标）。**坑：托盘脚本会覆盖 4 个带手工后处理的图标**（`tray/buddy-off|buddy-on|ghost-on|mark-on.png`，ed99be6 的辉光/补白修复只提交了 PNG、未回写脚本），跑完务必 `git checkout HEAD -- src-tauri/icons/tray/{buddy-off,buddy-on,ghost-on,mark-on}.png` 恢复

## 2. 项目是什么

**Satelite**（`com.satelite.proxy`）— 轻量级桌面代理客户端，Tauri 2 桌面应用，支持**三内核**。

- **内核**：sing-box（默认）、Xray 与 mihomo（`settings.core_type` 全局切换），另有 **aether 副进程**（`CoreKind::Aether`，仅作 WARP 边车、不可作主核，见头部 2026-09-27 条目），均作为 bundled resource 随应用分发（**不是** Tauri sidecar；由应用代码解压/下载/拉起）。Xray 另需 geosite.dat/geoip.dat（v2ray 格式），Windows TUN 需 wintun.dll；mihomo（标准 Clash Meta）自带 Clash REST API，另需 `<data>/mihomo/` 下的 Country.mmdb + GeoSite.dat（MetaCubeX mrs 格式，**与 Xray 的同名 dat 不通用、注意 GeoSite.dat 大小写**）；Windows TUN 用 `bin/wintun.dll`（与 Xray 共用）
- **后端**：Rust（`src-tauri/`），负责订阅解析、三内核配置生成、内核生命周期、系统代理、托盘、规则/DNS/连接数据
- **前端**：React 19 + TS + Vite（`src/`），玻璃拟态 UI，无路由库、无状态管理库、无 CSS 框架
- **平台**：macOS (arm64/amd64) + Windows x64 + Linux (amd64, AppImage)
- **包管理**：pnpm；前端端口 1420（strictPort）
- 语言：UI 中英双语（zh 默认）；代码注释中英混合

## 3. 目录速览

```
satelite-proxy/
├── src/                     # React 前端（~26.5k 行）
│   ├── api.ts               # ★ 前后端唯一桥：全部 invoke 封装（726 行）
│   ├── types.ts             # ★ 前端共享类型（与 Rust domain 对应，538 行）
│   ├── App.tsx              # Provider 栈 + ProShell/SimpleShell 切换
│   ├── pages/               # 专业模式 12 个页面
│   ├── ui/simple/           # 简洁模式 UI（独立 shell + 4 页）
│   ├── components/          # 玻璃设计系统 + 3D 首页 + 弹窗表单
│   ├── hooks/               # useVirtualRange / useVisibleInterval / 拖拽排序等
│   ├── i18n/                # zh/en 扁平文案表（TS 强制双语言键一致）
│   ├── theme/               # aerospace 深色 / day 浅色 + 6 主题色
│   └── App.css              # ★ 全部样式单文件（~7.6k 行，按 /* —— 段落 —— */ 分节）
├── src-tauri/               # Rust 后端（~36k 行）
│   ├── src/lib.rs           # ★ 入口：setup 流程 + 全部 command 注册
│   ├── src/commands/        # Tauri command 分层（按域拆文件）
│   ├── src/domain/          # ★ 核心数据模型（node/rule/dns/settings/subscription）
│   ├── src/state.rs         # AppState：全局状态中枢（1321 行）
│   ├── src/portable.rs      # 便携模式：portable.flag 检测，数据目录与 WebView2 目录重定向（见 §9.19）
│   ├── src/storage/store.rs # AppStore 持久化（JSON，含备份/迁移，2666 行）
│   ├── src/config/          # 配置生成：builder.rs（sing-box）+ xray.rs（Xray）+ mihomo.rs（mihomo/Clash YAML）+ dns_build/write/…
│   ├── src/core/            # 内核进程管理：kind.rs（CoreKind 三内核描述）、manager/download/assets/paths/提权/Job Object
│   ├── src/runtime.rs       # 编排：config→core→system proxy（~1600 行，含 Xray/mihomo 分支）
│   ├── src/api/clash_api.rs # Clash API 客户端（ureq + tungstenite）
│   ├── src/api/xray_metrics.rs # Xray metrics 客户端（/debug/vars 轮询）
│   ├── src/subscription/    # 订阅解析（clash/singbox/uri/manual）
│   ├── src/proxy/           # 系统代理（windows.rs / macos.rs / stub.rs）
│   ├── src/tray.rs          # 托盘
│   └── tauri.conf.json      # 主配置 + windows/macos-intel 覆盖
├── scripts/                 # 构建脚本（拉双内核/规则集、DMG、NSIS/MSI、图标生成）
└── src-tauri/tests/         # 集成测试（订阅解析 fixtures + live 下载测试）
```

## 4. 架构与数据流

```
React UI ──invoke()──▶ commands/* ──▶ AppState ──▶ storage(磁盘 JSON store)
                          │              │
                          │              ├─▶ config/builder.rs (sing-box) / config/xray.rs (Xray) / config/mihomo.rs (mihomo)
                          │              │      按 settings.core_type 生成 ─▶ <data>/config/active.json（JSON 系两内核共用）或 active.yaml（mihomo）
                          │              ├─▶ core/* 拉起内核进程（sing-box/xray: run -c active.json；mihomo: -f active.yaml -d <data>/mihomo）
                          │              └─▶ proxy/* 设置系统代理 (Win registry / macOS networksetup)
                          ▼
       sing-box 模式: api/clash_api.rs ◀──(HTTP/WS, ureq)── clash_api（连接快照/流量/延迟/选节点）
       mihomo 模式:   api/clash_api.rs ◀──(HTTP, ureq)── Clash REST API（同 sing-box 全量复用：热切/连接/测速/智能切换）
       Xray 模式:    api/xray_metrics.rs ◀──(HTTP, ureq)── metrics /debug/vars（仅流量总量；无逐连接/选节点 API，切节点=重启）
                          │
                          ▼
              conn_journal / state 缓存 ──invoke 轮询──▶ React UI
```

关键事实：

- **三内核**：`settings.core_type`（`singbox` 默认 | `xray` | `mihomo`）决定配置生成器与二进制；三套生成器共享 domain 模型、互不依赖（v2rayN 同款模式）。Xray 模式下切节点/切规则 = 重写配置重启进程、连接三页面无数据（前端显示占位提示）；mihomo 模式因 Clash API 兼容而**全功能**（热切节点、连接监控、delay 测速、智能切换均与 sing-box 同款）。
- **前端不直连 Clash API**。`src/` 里零 fetch/WebSocket，全部经 Rust command 中转；实时数据靠 `useVisibleInterval` 轮询 invoke + 5 个 Tauri 事件。
- **单窗口应用**。专业/简洁模式复用同一窗口，尺寸切换（960×720 ↔ 420×720）见 `src/ui/windowLayout.ts` 与 `src-tauri/src/window_ctrl.rs`；两模式均可自由放大（2026-09：pro 下限=设计尺寸 960×720、simple 下限 320×480，均无上限），放大后经 `useViewportScale` CSS zoom 等比放大 UI（简洁模式钉 420 居中，`data-ui-scaled` 任一维超设计即置位），窗口尺寸按模式持久化（localStorage + Rust 侧 `data/window_size_<mode>`，最大化状态不持久化；窗口出生即目标尺寸，见 §9.6）。
- **无路由**。导航是 `App.tsx` 里 `useState<NavKey>` + `TopNav`；次级页面 `React.lazy`（WebView 低内存重建）。
- **关窗进托盘**：`CloseRequested` 被拦截（`lib.rs:322-347`），可销毁 WebView 省内存但保活 Rust/tray/内核；`exit_allowed` 标志控制真正退出。

## 5. 后端模块详解（src-tauri/src/）

### 5.1 入口与生命周期

- `lib.rs` — `run()`：便携模式预检（`portable::patch_context`，见 §9.19）→ 插件注册（opener/dialog/deep-link/single-instance）→ setup（便携时先重建主窗口；加载 store 失败则弹窗退出）→ 托盘 → 启动 7 个后台任务 → 深链处理 → 静默启动/自动代理恢复。**全部 ~80 个 command 在 `lib.rs:348-431` 注册**，实现在 `commands/*.rs`（`commands/mod.rs` re-export）。
- 后台任务（均在 setup 中 spawn）：
  - `conn_journal.rs` — 轮询/WS 订阅 Clash 连接快照，维护活跃+历史连接环形日志。**三档变速（2026-09）**：`interval_for(可见, 需求, 帧大小)` 纯函数——UI 可见**且** 4s 内有连接数据命令心跳（`AppState::note_conn_query`，由 `live_connection_*`/`request_views` 入口记录）才 100ms，否则 1s；单帧 >1500 / >4000 连接（`last_snapshot_connections` 原子量）时 TUN 退避至 250/1000ms（mihomo ticker 对 interval≤0 会 panic，下限恒 ≥100ms）；变速重评估 2s 节流（interval 烧进 WS URL，变更=重连，防边界抖动刷socket）。ingest 每连接历史 key 只构建一次（`live_connection_keys` 与 `live_connections` 平行缓存）
  - `log_listener.rs` — 内核日志流（**仅 mihomo**）：订阅 Clash API `/logs` WS（warning 级，健康时零流量），解析 `logMetadataErr` 拨号失败行 `[TCP|UDP] dial <proxy> (match …) <src> --> <dst> error: …` → `Runtime::record_proxy_dial_failure`（有界环形，120s/512 条）并入 `passive_node_stats`。**为何 mihomo 独有**：mihomo 的连接 tracker 在拨号成功后才创建，拨号失败的连接从不进 `/connections`（连接日志对「节点 TCP 不通」全盲）；sing-box 在拨号前就挂进连接表，journal 已能看到（失败=零字节短命关闭，`request_looks_failed` 启发式：≤3s 快死 或 ≤15s 双向零字节=拨号超时带，2026-09）。归因：`proxy` 组→当前节点 tag、`node-*` 直存、DIRECT/REJECT/smart 池忽略
  - `subscription_auto.rs` — 按 `auto_update` 间隔定时刷新订阅（默认 1440 分钟）
  - `remote_rule_auto.rs` — 应用侧下载远程规则集缓存到本地，sing-box 只加载本地文件
  - `smart_switch.rs` — 智能选路（2026-09 二次重构，目标「必须快过手动 ping→试→换」）：**出口巡检**（`patrol_exit`）每 tick 经内核 URL 探测当前出口——快档 2.5s 走探测缓存、失败再以 5s 宽容档复核，抖动/略慢不触发、死出口一个 tick 内即发现且不依赖用户流量 → 死出口（双档均败）**绕过 dwell/cooldown 直入恢复扫描**（旧版切换后 120s 全盲、坏节点踩坑后干等是实战痛点）：候选按评分排序（缓存延迟+被动失败率+弹出惩罚）→ `SCAN_BATCH=8` 分批 `probe_nodes_ranked`（TCP-capable=TCP ping、QUIC-only=URL 兜底）做**排序预筛** → 每批 ping 最低的 `SCAN_VERIFY_TOP=3` 个经 `probe_nodes`（内核 delay API，unified delay）**实测验证**——绝不凭 ping 上位，「TCP 活但代理死」节点在此现形；ping/verify 双层失败均 `eject` 递增弹出（30s→120s→600s→1800s），坏池子多轮扫描严格前向推进，单轮预算 `SCAN_MAX=24` → 取最低验证延迟者 `apply_verified_switch`（Clash API 热切，秒级）。正常路径=被动劣化感知（sing-box=连接日志零字节关闭，mihomo=内核日志拨号失败，见 `log_listener`/`request_looks_failed`；软劣化有「同伴中位带宽」豁免）+ 600s 低频漂移校正，仅当验证候选以容差（50ms 绝对或 25% 相对，`should_prefer`）胜过当前出口 URL 延迟才切换；dwell 120s+cooldown 90s **只约束这类优化切换**（passive_hard 可放行）。**URL 延迟是唯一对比货币**，TCP ping 仅为排序预滤（消除了旧版 URL-vs-TCP 双口径的取舍）。`select_best_now`（开启智能时的 bootstrap）同走验证挑选（`ScanGoal::BestOverall` 扫满预算取最低验证延迟，当前节点参选、胜出即保持）。候选 fail-rate 评级走 `Runtime::passive_stats_for_tags` **单趟扫描**（O(历史+节点数)；逐节点全扫是 O(节点数×3000)，千节点订阅不可接受，2026-09）。**活动日志（2026-09-13）**：全部探测活动以 target=`smart_switch` 写应用日志（Logs→应用日志）——巡检慢档复核/扫描起止（含 reason=recovery|passive …degrade|drift re-probe|bootstrap 与 goal）/逐批 ping+verify 明细/节点弹出（次数+时长）/切换与保持决策均为 **info 级**（默认过滤级别即可见，可搜 `smart_switch`）；常规健康 tick 的信号摘要=debug、单次巡检心跳=trace，避免刷屏。**启动边沿自举（2026-09-13）**：`spawn` 循环以 `RunningEdge` 跟踪内核 Stopped→Running 边沿（停核时轮询加密至 5s；重启的停/起藏在 core transition 窗口内=`None` 无观察样本，读作连续运行**不触发**）——边沿命中且 `auto_select=smart` 时自动跑 `select_best_now` bootstrap，修复「应用重启后带着持久化智能模式启动代理、引擎不接管」（UI 只在切换到智能那一刻调 `smart_switch_now`，此前启动路径完全无 bootstrap）
  - `rule_apply.rs` — 规则变更的 500ms 防抖合并 + 全局串行 apply-and-restart
  - `state.rs::spawn_core_watchdog` — 内核看门狗：running→error 意外退出（非用户停止）经 `rule_apply::request_forced_restart` **真重启**（`restart_after_unexpected_exit`，仅缓存态=Error 的死核可复活；用户主动停止落在 Stopped 永不自动拉起），10 分钟滚动窗口内最多 3 次防配置错误死循环；决策逻辑 `watchdog_should_restart`/`should_revive_dead_core` 纯函数有单测（背景：曾有机静默 exit(1) 的实战事故；**注意普通 `request_restart` 路径对死核是空转**——`restart_if_running` 要求核心在运行，watchdog 必须走 forced 入口）。每次轮询还把 running/状态/副进程的**任何变化边沿** emit 为 `core-status-changed` 事件（payload `{running, core_state, sidecar_running, sidecar_kinds}`，kinds 为存活副内核列表），前端收到即 `refreshProxyStatus()`，消除隐藏窗口/锁忙/captureBusy 跳过轮询时的状态盲区。**同时独立盯全部副进程**（`poll_sidecar` → `Runtime::poll_sidecars`，Xray/mihomo 各自独立边沿/预算跟踪（`SidecarWatch`）；仅主核运行时才触发整体重启）
- `main.rs` — 仅调 `run()`。

### 5.2 状态与存储

- `state.rs` — `AppState`（managed state，`Mutex<AppStore>` + runtime 句柄 + pending 深链 URL + UI 可见标志）。几乎所有 command 走 `state.with_store(...)` / `with_store_mut(...)`。`start_proxy` 失败时会把 `status_cache` 写成 Error+错误文本（`mark_cached_core_error`），失败启动不再在缓存里停留 Starting。
- `storage/store.rs` — `AppStore`（serde JSON）：`subscriptions`、`nodes`（StoredNode）、`settings`、`dns`、`rule_sets`、`node_aliases` + 4 组 `retained_*`（**解析不了的新 schema 数据写回而非丢弃**）。含 `store.backup.json` 备份、损坏快照保留、schema 迁移（如 capture_mode/auto_select 迁移）。**v11（2026-09-23）**：`migrate_subscription_scoped_node_ids` 把全部节点 id 重算为订阅作用域哈希（`ProxyNode::scoped_id`）并经 `remap_node_refs` 重映射 current_node_id/rule_sets 内外 node_id(s)/pools/chains/favorites（写 `store.pre-v11.backup.json`）；`remap_node_refs` 亦被 `update_node_from_draft`（单节点编辑，见 §5.8）复用——身份变更轮换 id 时引用随行、收藏迁移、身份未变保留延迟、Xhttp mode/extra 回填防丢，编辑是临时的（订阅刷新整组重建即覆盖）。
- 磁盘布局（`app_data_dir`）：
  - `store.json` — 主存储；`store.backup.json` — 备份
  - `config/active.json` — JSON 系内核（sing-box/Xray）的运行配置（**两内核共用同一文件，有意为之**：每次启动由当前内核的生成器整体重写，tmp+rename 原子写，带时间戳备份；内容从不跨内核混用）；`config/active.yaml` — mihomo 的 Clash YAML（同款整体重写策略）；`config/xray-sidecar.json` — Xray 副进程配置、`config/mihomo-sidecar.yaml` — mihomo 副进程配置（§9.20，仅对应委托开启时生成）；**custom 运行时配置= `config/custom-<id>.<json|yaml>`（2026-09-25 起三类型：singbox/xray→json、mihomo→yaml；必须放 config/ 顶层——mihomo 的 `-d` home 由 config 路径 `parent().parent()/mihomo` 推导，嵌套子目录会指错 home）**，绝不写 `active.*`
  - `subscriptions/<id>.txt` — **订阅原始报文快照（2026-09-25）**：每次导入/刷新把 url/file/text/custom 的原文落盘（`ImportOutcome.raw_body` → `persist_import_replacing` 写入，`get_subscription_raw_config` 读取供「查看配置」弹窗；旧订阅在下次刷新后才有，text/custom 无文件时回落 store 内 content；node 档无正文）
  - `bin/sing-box(.exe)` + `version.txt` — sing-box 内核；`bin/xray(.exe)` + `xray-version.txt` — Xray 内核；`bin/geosite.dat`/`geoip.dat`/`wintun.dll` — Xray 资产；`bin/mihomo(.exe)` + `mihomo-version.txt`（`core/paths.rs` + `core/assets.rs`）
  - `mihomo/` — mihomo home 目录（`-d` 参数）：`Country.mmdb` + `GeoSite.dat`（MetaCubeX mrs；**不能放 bin/**，与 Xray 的 v2ray 格式 geosite.dat 同名不通用；**GeoSite.dat 大小写敏感**，macOS 下必须精确命名）
  - `logs/` — 应用日志（`app_log.rs`，`log_retention.rs` 清理）
  - 远程规则集缓存（`.srs`）

### 5.3 数据模型（domain/）★ 改功能先看这里

| 文件 | 内容 |
|---|---|
| `domain/node.rs` | `Protocol`（SS/VMess/VLESS/Trojan/Hysteria2/TUIC/AnyTLS/SOCKS5/**Masque**(mihomo 独占)/**Unknown**(2026-09-25：mihomo 原生未建模类型 ssr/mieru 的救援收纳，仅 raw 透传、协议层无任何内核支持)/…）、`ProxyNode.raw`（clash 解析路径给每个节点附着的**原始 YAML 条目**——mihomo 生成器按原文透传实现零信息损失，Unknown 节点的唯一真源；id 对 Unknown 走 raw 哈希）、`ProtocolConfig`、`TlsConfig`、`Transport`、`ProxyNode`（id 生成 **v11 起为订阅作用域**：`scoped_id(scope)`=hash(订阅id\|server\|port\|协议\|凭据)，同订阅刷新 id 稳定、跨订阅同后端不撞 id——同 URL 可多订阅的前提；`with_computed_id` 无作用域版仅 custom singbox 抽取节点还在用）、`ParseResult`、`ManualNodeDraft`（表单模型） |
| `domain/settings.rs` | `AppSettings`（~40 字段：端口/TUN/capture_mode/outbound_mode/auto_select/locale/theme/accent/hero_style/tray_icon/**tls_fragment_singbox/tls_fragment_xray**…）、`OutboundMode`、`CaptureMode`、`AutoSelectMode`、`ExtraInbound`、`RuntimeSource` |
| `domain/rule.rs` | `Rule`、`RuleSet`（本地/远程/内置，ownership/strategy/dns_strategy；strategy 6 值 proxy/direct/block/node/filter/smart，node=整组指定节点、filter=整组关键词过滤池，参数存集级 node_id/smart_include 等字段；node 可多选：`node_ids` 2+ 勾选=整组显式节点池 `is_node_pool()`，生成/维护与 Filter 池同路径同 tag `smart-<集id>`，勾 1 个仍存单钉 node_id 形状）、`RuleType`、`RuleTarget`、`BUILTIN_REMOTE_RULE_SETS`（3 条内置远程规则，需与 `scripts/fetch-bundled-rule-sets.sh` 同步） |
| `domain/dns.rs` | `DnsSettings`（含 `dns_final` 兜底与 `remote_dns` 用户远程 DoH 池，`effective_remote_pool()` 为生效池唯一入口）、`DnsRule`、`DnsAction`、FakeIP、Hosts 配置 |
| `domain/subscription.rs` | `Subscription`、`SubscriptionSource`（url/file/text/node/**custom**）、`SubscriptionView`。**自定义配置多核化（2026-09-25）**：原 `Singbox{content}` 泛化为 `Custom{content, kind}`——`CustomConfigKind`（singbox/mihomo/xray）由 `subscription::detect_custom_config_kind` 从正文判型（JSON 系按 inbounds/outbounds 条目 `type`（sing-box）vs `protocol`（Xray）区分、YAML 按 Clash 顶级键判 mihomo），serde 兼容旧 `singbox` wire（读回为 `Custom{singbox}`，新写 profile=`custom`+`config_kind`）；`SubscriptionView/Detail.custom_kind` 下发类型标签；custom 档 `format` 字段=kind 字符串。**同 URL 允许多次订阅（2026-09-23）**：id 由 add 命令经 `services/import.rs::unique_subscription_id` 保证唯一（源哈希首选、撞库加熵），`update_subscription` 恒保自身 id 不再合并；**显示名不含 id（2026-09-23 收敛）**——`display_name()` 已删除，`to_view().name` 与 `ListedNode.subscription_name` 均为干净名，id 由前端悬停（`title`）与订阅分组组头显示（`nodeGroups.ts` 按 `subscription_id` 分组、标签 `名称(id:末4位)`），`to_detail` 原名供编辑回填 |

### 5.4 配置生成（config/）★ 三套独立生成器共享 domain 模型

- `builder.rs` — ★ sing-box 生成器：`ProxyNode[] + AppSettings + RuleSets + DnsSettings → sing-box JSON`（`BuildOptions`，含 pools/chains/**sidecar**）。inbounds（mixed/Clash API/多监听/TUN）、outbounds（含 urltest/手动选择 selector）、route 规则编译都在这里。节点池 → selector（空池跳过；Node 多选池 `explicit_set_pool_tags` 与 Filter 池同走 `smart-<集id>` 整组 selector，本地集经 route_local_set_grouped 的 pool_key、远程集经 remote_set_route_rule，成员按历史延迟排序）；代理链 → detour 链（**规则指向末跳=出口，hop[i≥1].detour=hop[i-1]，hop0 由客户端直拨**——方向弄反会导致 [美→港] 链显示美国 IP；正向发射满足先定义后引用；**i≥1 的池跳**必须展开为逐成员克隆 + 链内 selector——共享 selector 的成员无法表达"经前一跳拨出"；**i=0 池跳**复用共享 selector 即正确，客户端直拨）。链路诊断专用出口（**仅存在 ≥1 条可解析链路时**生成）：回环 mixed 入口 `diag-in`（**127.0.0.1:26486**，`DIAG_INBOUND_PORT`）+ `chain-diag` selector（成员=各链出口 tag+direct）+ 置于 sniff 后、用户规则前的 `inbound` 路由规则——`diagnose_chain` 经 Clash API 热切该 selector 后从诊断入口抓 ip.sb，实现**零规则、零重启**的真实出口验证；端口冲突会让内核启动失败并报 26486。`BuildOptions.sidecar: Option<SidecarPlan>`（副进程委托，§9.20；条目 `SidecarPort{node_id, port, kind}` 带目标内核，端口单一连续空间两副进程混排）：命中计划的节点**保留原 tag**（`node-<id16>`）但改发 `type:"socks"` 出站指向 `127.0.0.1:<副端口>`——selector 成员/规则钉定/smart 池/Clash 热切/delay 测速全部零改动照常工作。sing-box 不认识的协议走 §9.21 过滤模型：masque 未委托时生成期报错过滤（错误文案指引钉到 mihomo 或切内核），与 xhttp 同款。**TLS 分片**（`BuildOptions.tls_fragment_singbox`，设置页「内核设置→TLS 分片」）：开启后对全部 `tls.enabled` 的节点出站（含链路克隆）打 `tls.fragment:true`（sing-box v1.13.18 该开关位于出站 `tls` 对象内、布尔型，真机 `check` 已验证；拨号字段里的 `udp_fragment` 是无关功能），仅 sing-box 主核模式生效。
- `xray.rs` — ★ Xray 生成器（参照 v2rayN `CoreConfig/V2ray/*`）：mixed/tun inbounds + sniffing、vmess/vless(flow)/ss/trojan/socks/http/wireguard outbounds + streamSettings（tls/reality + ws/grpc/httpupgrade/xhttp）、routing（`full:`/`domain:`/关键词/geosite:/geoip:/process 映射、balancer+observatory=kernel 自动选路）、DNS 出口分流（远程 DoH 经主出站，dns-module/direct-dns inboundTag 规则；非 final 池 `skipFallback` 仅作分类，见 §18②）、stats/metrics（`/debug/vars`）。无 selector 出站——主目标=选中节点 tag 或 balancer，**切节点即重启**。REALITY 仅支持 tcp/grpc/xhttp 传输（ws/httpupgrade 组合在生成期报错跳过）；**h2/http 传输已被 Xray v26 在配置层移除**（`network:"http"` 即拒载，生成期报错跳过 + `supports_node` 列表过滤，sing-box/mihomo 仍原生可用，2026-09）。xhttp 的 `extra` 调优参数（分享链 base64url `extra=` / clash `xhttp-opts.extra`）解析入模型并原样嵌入 `xhttpSettings.extra`，非法 JSON 生成期丢弃记 warn（Xray 对坏 extra 整配置拒载）。用户自建远程 `.srs` 集**跳过**（Xray 不识别），内置 3 条映射为 geosite/geoip。`skip-cert-verify` 节点不输出 `allowInsecure`（Xray ≥ 26 已移除该字段，输出会导致整个配置加载失败），证书校验保持开启并记录告警。**TLS 分片**（`tls_fragment_xray`）：Xray 无出站级开关——`apply_xray_tls_fragment` 追加 freedom `fragment-out` 出站（`settings.fragment`=`tlshello`/length 100-200/interval 10-20ms）并给 `security` 为 tls/reality 的节点出站打 `streamSettings.sockopt.dialerProxy=fragment-out`（无 TLS 节点天然跳过）。另有 `build_xray_sidecar_config`：**Xray 副进程最小配置**（§9.20）——每委托节点一个 `127.0.0.1:port` mixed 入站（tag `in-sc-<port>`）+ 该节点常规出站（tag 同主配置 `node-<id16>`）+ 1:1 `inboundTag→outboundTag` 路由 + final 兜底（带 `tls_fragment: bool` 参数，与主配置共享 `apply_xray_tls_fragment`——委托节点的真实拨号在副进程，分片随行）；**无** Clash API/TUN/DNS 模块/metrics/geodata 引用（因此启动不需要 geosite.dat/geoip.dat）。
- `mihomo.rs` — ★ mihomo 生成器（Clash YAML，`serde_yaml::Mapping` 保序；字段名以自家 `subscription/clash.rs` 解析器为逆向权威）。**原文透传（2026-09-25）**：`mihomo_proxy_mapping` 对带 `raw` 的节点原样输出 provider 条目（仅把 `name` 重写为 `node-<id16>` tag）；provider 自带 `dialer-proxy` 按原名→tag 重写、引用缺失则整节点跳过（防 mihomo 拒载）；主组/池/规则照旧引用 tag。ss(+obfs/v2ray-plugin)/vmess(全传输)/vless(REALITY+Vision，uTLS 完整)/trojan/hysteria(2)/tuic/wireguard/anytls/snell/**masque**(usque 式 ECDSA 私/公钥 + ip/ipv6/mtu/network(quic|h2|h3-l4proxy)/congestion-controller；TLS 固有——只写 sni/skip-cert-verify **不走 apply_tls**，不输出 `tls:` 字段)/socks5/http/ssh。主组 `proxy`（select，选中节点排首位；kernel 模式=全节点 url-test）保持 sing-box 的 Clash API 契约（热切 `PUT /proxies/proxy`）；filter 池/Node 多选池与 smart 池均为 select 组、标签 `smart-<id16>`（应用侧智能切换 PUT 维护）；内置 3 条映射 `GEOSITE,cn`/`GEOIP,cn`/`GEOSITE,geolocation-!cn`；bypass_lan 用显式私有 CIDR；block_quic 用 AND 逻辑规则；DNS 池取自 `domain/dns.rs`（远程池经 `effective_remote_pool()` 可被用户自定义覆盖，见 §18①；`nameserver`=dns_final 池、**不写 `fallback`**，见 §18②）+ nameserver-policy(+.suffix/geosite:cn)+hosts+fake-ip（TUN 强制）+ `proxy-server-nameserver`；`find-process-mode` 接 AppSettings.find_process（strict/off）；extra_inbounds 走 `listeners`。仅 Naive/Tor/独立 ShadowTls 协议与 ss+shadow-tls 组合被过滤。用户自建 `.srs` 集跳过。另有 `build_mihomo_sidecar_config`：**mihomo 副进程最小配置**（§9.20）——每委托节点一个 loopback mixed listener（`in-sc-<port>`，`udp: true`）+ **`proxy:` 字段按监听器钉出站**（mihomo 侧等价 Xray 的 inboundTag→outboundTag 1:1 派发）+ `MATCH,DIRECT` 兜底；**无** api/tun/dns/geodata 引用（`-t` 校验已实测空 home 可过）。
- `dns_build.rs` — sing-box 1.12+ `dns` 对象：解析器池（取 `domain/dns.rs` 共享池常量；sing-box 单 tag 无竞速，**两池均只发 pool[0]**，见 §18①）、统一规则集选解析器、Hosts predefined server、FakeIP。
- `write.rs` — 原子写 `active.json`（JSON 系两内核共用同一文件）与 `active.yaml`（mihomo）；`xray-sidecar.json` 与 `mihomo-sidecar.yaml`（两副进程配置，tmp+rename 无备份）；custom 配置原样持久化。
- `rule_files.rs` / `dns_files.rs` — 规则/DNS 落盘为 sing-box 引用的文件。
- `custom.rs` — 自定义 sing-box 配置的检查（`inspect_singbox_config`）。
- `punycode.rs` — 域名 punycode。

### 5.5 内核管理（core/）— 三内核（sing-box / Xray / mihomo）

- `kind.rs` — ★ `CoreKind` 描述符：binary 名、GitHub repo、release 资产命名（**三内核命名规则不同**：sing-box `sing-box-1.13.15-darwin-arm64.tar.gz` vs Xray `Xray-macos-arm64-v8a.zip` vs mihomo `mihomo-darwin-arm64-v1.19.30.gz` 裸 gz 二进制（darwin/linux）；**mihomo 的 amd64 资产一律加 `-compatible` 中缀**（GOAMD64=v1），见 §9.17⑦）、版本参数与输出解析（mihomo `-v` → `Mihomo Meta vx.y.z …`）、`check_command_args`/`run_command_args` 完整参数构造（sing-box/Xray `run -c`；mihomo `-f <file> -d <home>`，home 从 config 路径推导）、spawn env（Xray 设 `XRAY_LOCATION_ASSET`；mihomo 无需 env，wintun.dll 放 exe 同目录）、日志前缀、协议支持集（`Protocol::xray_supported`/`mihomo_supported`）。**`supports_node` 的 raw 放行（2026-09-25）**：Mihomo + `node.raw` 存在 → 直接放行（Naive/Tor/ShadowTls 除外，内核无此出站）——原文对 mihomo 是权威，模型只是投影；sing-box 的 `supports` 同步排除 Unknown。
- `manager.rs` — 进程生命周期（`CoreKind` 参数化）：sing-box `check -c` / Xray `run -test -c` / mihomo `-t -f` 校验 → 启动；**Xray + TUN 跳过预校验**（Xray 的 `-test` 会真建 tun 网卡需管理员，未提权必失败 exit 23；mihomo 的 `-t` 不建网卡可正常预校验）；Windows `CREATE_NO_WINDOW`；CoreState 状态机；优雅停止；TUN 提权链路内核无关（helper 按二进制名推断 kind）。
- `download.rs` — GitHub Releases 下载/更新（按 kind 选 repo/资产/提取目标；Xray zip 额外提取 geodata；sing-box Windows zip 额外提取 `libcronet.dll`；mihomo zip 仅含版本化 exe）。**最新版检测不碰 api.github.com**：与自检同源走 `github.com/<repo>/releases/latest` 页面 302 跳转取 tag（`fetch_latest_core_tag_via_redirect`），资产 URL 由 tag 确定性构造（`release_info_from_tag`，size 恒 0 下载时才知）；跳转失败回落 pinned fallback 版本；按 tag 下载零网络请求。
- `assets.rs` — Xray 资产：`ensure_geodata`（staged→bundled→Loyalsoldier v2ray-rules-dat 下载）、`ensure_wintun`；mihomo 资产：`ensure_mihomo_geodata`（`<data>/mihomo/` 的 Country.mmdb + **GeoSite.dat**（精确大小写）；staged→bundled→MetaCubeX/meta-rules-dat 下载——缺失时 mihomo 会经未启动的代理自下载而超时，故必须预置）；Windows sing-box：`ensure_libcronet` 启动兜底（**naive 出站运行期从 exe 目录动态加载 Cronet，DLL 缺失则含 naive 节点的配置启动即 FATAL**——新装由 staging/zip 提取落位，存量 bin 已有二进制不再走 stage，由此补齐；两处 sing-box 启动路径均调用）。`prefetch_runtime_assets(kind,…)` 在**内核下载/更新完成后**即时预取该内核的运行时依赖（Xray→geodata、mihomo→geodata、两者 Windows→wintun；走下载代理，失败仅 warn），启动时的 `ensure_*` 降级为兜底（§9.22）。
- `job.rs` — Windows Job Object 绑定子进程，父进程异常退出时内核随之死亡（防端口占用残留）。
- `elevate.rs` / `macos_auth.rs` / `macos_net.rs` — TUN 提权（Windows UAC / macOS 授权）。
- `memory.rs` — 内存占用探测（Windows 用 NT 进程表 RSS）。
- `paths.rs` — 内核二进制/版本文件路径解析（resource 目录 → data 目录 staging；bundled 布局三内核共用 sing-box 式平台目录名，release 资产名才按 kind 区分；Windows staged sing-box 同步落位 `libcronet.dll`，见 `assets.rs` 条目的 naive 说明）。sing-box 保持 `bin/sing-box`+`version.txt` 存量布局；Xray 用 `bin/xray`+`xray-version.txt`；mihomo 用 `bin/mihomo`+`mihomo-version.txt`，home 目录 `mihomo_home_dir` = `<data>/mihomo`。

### 5.6 运行时编排与外部 API

- `runtime.rs` — `Runtime`/`ProxyStatus`（含 `core_type`/`sidecar_running`）：按 `settings.core_type` 分支 config 生成 → 写盘 → core 启停 → 系统代理联动；连接视图缓存与 delta（`LiveConnectionBatch` revision 机制）。**启动就绪判定必须实测端口**（2026-08）：sing-box/mihomo health 等待成功条件 = Clash API `/version` 应答 **且** `dial_mixed_ok()`（mixed 端口 TCP 拨号实测，`readiness_failure_detail` 区分「API 未应答」与「API 活但入站未监听」两种失败）；sing-box 两路径（生成/custom）统一走 `wait_clash_api_ready`（TUN 基础 12s / 非 TUN 6s；检测到 sing-tun 慢网卡 WARN `open interface take too much time to finish!` 且进程存活时证据驱动延长至 45s 硬上限——clash_api 排在 tun inbound 之后启动，wintun 网卡慢时 10s 处掐核属于误杀，且杀/重启反复churn网卡更慢，2026-08 实战事故；失败时经 `map_slow_tun_start_hint` 在含日志尾的完整错误上追加 wintun 冲突指引），mihomo 仍为内联 TUN 10s / 非 TUN 6s；Xray 就绪窗口同为 6/10s、成功条件 = 进程存活 **且** mixed 拨号成功（metrics 命中仅 best-effort warn，不作为就绪条件）；custom 配置路径不拨号（入站形状任意）。**custom 启动三类型分支（2026-09-25）**：`start_custom_proxy` 按 `CustomConfigKind` 分派——singbox 走原路径（重校验+`inspect_singbox_config`+libcronet 兜底+有 clash_api 则 `wait_clash_api_ready`、否则进程存活 4s）；mihomo=`start_custom_mihomo`（`validate_custom_mihomo_config`+`inspect_mihomo_config`（mixed-port>port>socks-port>listeners 端口、external-controller/secret、tun.enable）+ `ensure_mihomo_geodata` + 写 `config/custom-<id>.yaml`——放 config/ 顶层是让 `mihomo_home_args` 推导出共享 `<data>/mihomo` home；有 external-controller 则 Clash API 等待并接 conn_journal/热切，与生成路径同款）；xray=`start_custom_xray`（`validate_custom_xray_config`+`inspect_xray_config`（socks/http inbound 端口、tun inbound）+ `ensure_geodata`/wintun + 进程存活判就绪、无 API）；macOS 提权预判 `resolve_pending_elevation` 同样按类型选内核。失败信息统一附 `core_startup_log_hint` 日志尾并过 `map_tun_permission_hint`（`manager.rs`，pub(crate)）。Xray 分支 `start_xray_proxy`：ensure geodata/wintun → `build_xray_config` → 按上述端口拨号判定就绪，`xray_metrics` 替代 clash_api。mihomo 分支 `start_mihomo_proxy`：ensure mihomo geodata → `build_mihomo_config` → 写 active.yaml → 与 sing-box 同款「ClashApi health + mixed 拨号」等待（`self.api` 即 clash_api，conn_journal/热切/智能切换全复用）。`build_options()` 为三生成器共享的 BuildOptions 构造器。sing-box 分支支持 **双副进程委托**（§9.20，Xray 与/或 mihomo）：`compute_sidecar_plan`（settings+chains+nodes → 按协议钉选分组的多副进程计划，commands 预览同款复用；端口 `base+i` 单一连续索引空间两副进程混排防撞）→ 主核 health OK 后按计划中出现的 kind 逐个 `start_sidecar`（写 `xray-sidecar.json` / `mihomo-sidecar.yaml`，经 `Runtime.sidecars: Vec<SidecarProc>`——每副进程独立 `CoreManager`，结构体无静态状态可直接并存；mihomo 副进程 home 由 `mihomo_home_args` 按配置路径推导为共享的 `<data>/mihomo`，副配置无 geodata 引用无需预置）；停/重启/退出先停全部副进程，主核启动失败或任一副进程启动失败均整体回滚（含已启动的另一个副进程）。
- `api/clash_api.rs` — Clash 兼容 API 客户端（sing-box 与 mihomo 模式共用；热切需 `Content-Type: application/json`，`send_json` 已带）。**HTTP 用 ureq（非 reqwest::blocking，避免嵌套 Tokio runtime panic，见文件头注释）；WS 用 tungstenite 仅握手**。WS URL 构造：`connections_ws_url(interval)`（连接快照流）与 `logs_ws_url(level)`（内核日志流，log_listener 用）。
- `api/xray_metrics.rs` — Xray 模式 metrics 客户端：轮询 `/debug/vars` 汇总 `stats.outbound[*].uplink/downlink` → TrafficTotals（connections 恒 0；无逐连接 API）。
- `state.rs` `select_current_node_serialized` — sing-box/mihomo 走 clash select_proxy 热切换（组名 `proxy`）；Xray 无 API → 持久化后返回 restart_needed，由 `rule_apply::request_restart` 重启生效；不支持的节点形状直接报错。
- `services/core_support.rs` — 订阅「内核支持详情」报告（2026-09-25）：`core_support_report(body)` 纯函数=当前解析器重 parse + 逐节点过三内核 `supports_node`，命令 `get_subscription_core_support` 读已存原始报文计算（node 档无正文返回 None）。
- `services/latency.rs` — 测速：TCP 协议直连 server:port（内核无关）；UDP 系协议（hysteria2/tuic）走 Clash delay API（sing-box/mihomo 有此 API；Xray 模式下此类节点本就不被支持）。批量探测按输入顺序起测（并发槽空出即从前向后补位）；`probe_nodes_streaming`/`ping_nodes_streaming` 带 `on_result` 回调，每个探测完成即刻回调（commands/latency.rs 据此经 Tauri `Channel<LatencyResult>` 逐节点推给前端）。探测共享结果缓存（成功 30s / 失败 15s，per-key 在途合并；**这也是 smart_switch 后台排序/健康探测读的缓存**）；手动触发的三个测速 command 一律 `use_cache=false`——不读缓存每次真测，结果仍写回缓存供后台复用（2026-08）。落库统一走 `update_node_latency` 优先级入口并推送 `node-latency-changed`（详见 §7「改测速」）。
- `services/import.rs` — 订阅 URL 去重键、导入文件读取。
- `services/dns_diag.rs` — ★ 内核级 DNS 诊断（DNS 页「诊断」，command `diagnose_dns`）：双层设计——① 实时查询：sing-box/mihomo 经 `ClashApi::dns_query`（`GET /dns/query?name=&type=A`，sing-box 走完整 DNS 规则链、两内核响应同构且都不返回上游 server）；② 路径推演：`DnsPathAnalyzer` 在应用侧复刻三生成器的决策链（规则集 stored order → Hosts → DNS 页规则 → FakeIP → dns_final，含 §18 分歧点：mihomo 关键词回落 nameserver、Block 集仅 sing-box 拒绝 DNS、xray/mihomo 跳过用户 .srs、内置 geosite 集按本地 .srs 缓存近似判定 approx）。远程集匹配复用 `srs.rs`/source JSON，缓存路径校验同 `list_remote_rule_items`。Xray 无 DNS API → 仅路径推演 + query_note 说明。
- `services/exit_ip.rs` — 出口 IP 探测（首页「网络探测」卡，command `check_exit_ip`，2026-08）：4 个公共 IP API（api.ip.sb/geoip、ipwho.is、ip-api.com、api.myip.com）`spawn_blocking` 并发竞速、mpsc 收首个成功，其余靠 ureq 超时（connect 5s/总 9s）自行收敛，整体 12s 兜底；浏览器 UA（ip.sb 会 403 裸客户端）。`via_proxy=true` 时 agent 挂 `ureq::Proxy` 指向 `127.0.0.1:<mixed_port>`（三内核的 mixed 入站都支持 HTTP 代理语义，请求按用户规则出站→答案即当前出口）；内核未运行或 `outbound_mode=direct` 时直连探测（返回本机公网 IP）。解析器归一化各源 JSON 字段差异（`parse_source` 有单测）。前端 `DashboardPage` 点击卡片同时刷新延迟+出口 IP，并在 statusReady 后按 `running:nodeId` 键变化（内核启停边沿/节点切换）自动重探，版本号防过期结果覆盖。
- `srs.rs` — `.srs` 二进制规则集结构解析（LOUDS trie），供列表/计数/校验（`list_remote_rule_items` 的后端；固定用 sing-box 二进制 decompile）。
- `smart_switch.rs` / `rule_apply.rs` / `remote_rule_auto.rs` / `builtin_remote_rules.rs` — 见 5.1。smart_switch 在 Xray 模式禁用（依赖连接日志；mihomo 有连接日志不受限）。
- `conn_journal.rs` — 连接日志（活跃快照 + 已关闭请求历史 + 失败请求），`list_connections/list_connection_changes/list_requests/list_request_failures` 的数据源；Xray 模式降级为 metrics 轮询（仅流量）；mihomo 模式与 sing-box 同款全量。

### 5.7 系统集成

- `proxy/windows.rs|macos.rs|stub.rs` — 系统代理设置（注册表 / networksetup），含 owned-proxy 标记与崩溃残留清理（启动时 `cleanup_stale_system_proxy`）。
- `tray.rs` — 托盘菜单 + 图标状态刷新（10 种托盘图标 `TrayIconStyle`，`src-tauri/icons/tray/`；saturn=应用图标同款土星瓦片（2026-09-24 起随 icon-frost 联动），off 灰/on 彩色；saturn_classic=霓虹前的 facenew 土星手工归档（与 saturn 组 2026-09-24 起不再重复）；saturn_classic=霓虹前的 facenew 土星手工归档）。**菜单文案双语（2026-09-21）**：`TRAY_LABELS_ZH/EN` 按 `settings.locale` 选取，`refresh_icon` 内 `refresh_labels` 全量重设（`update_settings` 无条件调 `refresh_icon`，故 UI 切语言托盘即时跟随）；新增菜单项时两处常量表都要加。
- `window_icon.rs` — **Windows 标题栏图标修复（2026-09-23）**：Tauri 默认窗口图标=从 icon.ico 抽单帧 RGBA → tao `CreateIcon` 一个大尺寸 HICON → `WM_SETICON(ICON_SMALL)`，标题栏 GDI 硬缩必糊（ico 多尺寸条目根本用不上）。本模块在窗口（重）建后/Focused/ScaleFactorChanged 改设：ICON_SMALL/SMALL2=内嵌 `tray/titlebar-<16..48>.png`（badge 薄荷绿卫星标记，用户指定的标题栏图样）`CreateIcon` 原生尺寸零缩放，ICON_BIG=exe 图标资源组（tauri-build 嵌于 id 32512）按 DPI `LoadImage`（任务栏/Alt-Tab 保持土星应用图标）；(hwnd,dpi) 记忆去重使 Focused 钩子近零开销。挂点：`lib.rs` setup 全量 + `window_ctrl::show_main` 重建后 + `lib.rs` 窗口事件。仅 Windows，其余平台空实现。
- `window_ctrl.rs` — 窗口 show/hide/destroy（托盘内存管理）、ui_mode 偏好持久化；尺寸常量与前端 `windowLayout.ts` 对应。
- `url_scheme.rs` — 注册并抢占 `clash://` `sing-box://` `singbox://` 为默认（深链一键导入）。
- `autostart.rs` — 开机启动（macOS LaunchAgent）。
- `app_log.rs` / `log_retention.rs` — 自有日志系统（trace~error 分级、panic hook、保留策略）。
- `error.rs` — `AppError`/`AppResult`。

### 5.8 commands/ 分层（前端 invoke 的直接实现）

`config.rs`（订阅 CRUD/激活/mix、`generate/preview_singbox_config` 按 core_type 分发三生成器，mihomo 返回 YAML 文本；节点列表按 `CoreKind::supports_node` 过滤、`subscription_name` 为干净订阅名（id 经 `ListedNode.subscription_id` 供前端悬停/分组）；**`get_node_draft`/`update_node` 单节点参数编辑**——`node_to_draft` 预填、保存走 `store.update_node_from_draft`（id 轮换时引用/收藏/首选随行迁移，enabled 节点集变化防抖重启），编辑临时、订阅刷新覆盖）、`core.rs`（启停/重启/capture_mode/三内核下载更新/`set_core_type` 切内核/`refresh_geodata` 带 kind 参数——xray 刷 Loyalsoldier .dat、mihomo 刷 MetaCubeX mmdb/GeoSite.dat/`reset_core_to_bundled` 内核恢复出厂——删除用户下载的 `bin/` 副本回落 bundled（运行中被锁则按下载同款 `.previous` 改名让路），下次启动自动重新 stage，仅当重置的 kind 恰为运行中主核时才触发重启；无 bundled 副本时（默认安装仅打包 sing-box）前端改走 `download_core(kind, CoreKind::fallback_version())` 联网拉取出厂版本并按需重启，该版本经 `CoreInfo.factory_version` 暴露）、`chain.rs`（节点池/链路 CRUD + `list_chain_usage` 规则集引用计数 + `diagnose_chain` 逐跳诊断（单跳/链前缀探测，仅 sing-box、经 Clash delay API），编辑走防抖重启同 rules）、`connections.rs`（连接/请求/失败；`list_connection_changes` 增量协议：带 `lastOrderRevision`，纯计数更新不下发 `order_ids`）、`diagnostics.rs`（`diagnose_network` 检测、`check_exit_ip` 出口 IP 竞速探测→services/exit_ip）、`dns.rs`（DNS+hosts 设置 CRUD、`diagnose_dns` 内核级 DNS 诊断→services/dns_diag）、`latency.rs`、`logs.rs`（`list/clear_app_logs` + `get_core_log_tail(limit, kind)`——按 kind 读对应内核的 `logs/<prefix>-<hour>.log`，多核模式主核/副进程分开，`Runtime::core_log_tail_for`）、`proxy.rs`（状态/系统代理/TUN）、`rules.rs`（规则集 CRUD/排序/远程规则，1167 行）、`subscription.rs`（导入各来源；**add 系命令传 `unique_subscription_id` 新鲜 id（同 URL 可重复订阅）、`update_subscription` 不再按 URL 合并**；`add_subscription_singbox` 更名 **`add_subscription_custom`**（类型自动识别），新命令 **`get_subscription_raw_config`** 读订阅原始报文（url/file 读 `subscriptions/<id>.txt`、text/custom 回落 store 内 content、node 无正文）供「查看配置」弹窗、**`get_subscription_core_support`** 内核支持详情（→services/core_support）；`get_node_draft`/`update_node` 对 Unknown 原文透传节点拒绝编辑；`persist_import_replacing` 在落库后把 `ImportOutcome.raw_body` 写原始报文文件（失败仅 warn 不阻断导入））。command 名与 `src/api.ts` 导出一一对应（snake_case）。

## 6. 前端模块详解（src/）

### 6.1 骨架

- `main.tsx` → `App.tsx`：`ThemeProvider > LocaleProvider > UiModeProvider > ImportIntentProvider > AppShell`。
- `AppShell` 按 `mode` 选 `SimpleShell` / `ProShell`；监听 `config-apply-status` 事件驱动全局 busy 与错误 banner。
- `ProShell`：`useState<NavKey>`（dashboard|config|nodes|traffic|logs|settings）+ `TopNav`；页面 `React.lazy` + `key={nav}` 强制重挂载触发进场动画。
- `UiModeContext.tsx`（`src/ui/`）— localStorage `satelite.uiMode` 先行渲染防闪烁；切模式先调 `set_ui_mode_pref` 让 Rust 调窗口尺寸再换 shell。`UiModeMenu.tsx` — 工具栏 "⋯" 菜单（模式切换/切换内核 sing-box|Xray|mihomo/重启内核/复制代理环境变量）。

### 6.2 桥接层 ★

- `api.ts` — 全部 `invoke()` 封装。要点：
  - `updateSettings` 是 **60ms 批量合并写入器**；
  - `peekSettings/peekProxyStatus + keepSettings/keepProxy` 模块级快照，供页面重挂载时种子状态防闪默认值；
  - 生命周期类调用（start/stop/restart/capture/outbound）包 `trackCoreBusy()` 驱动导航栏 spinner。
- Tauri 事件消费：`config-apply-status`（App.tsx）、`core-status-changed`（App.tsx → `refreshProxyStatus()`；watchdog 发出的生命周期边沿推送，Dashboard/SimpleConnect/TopNav 经 `api.ts::onProxySnapshot` 订阅快照即时重绘）、`node-latency-changed`（`api.ts::onNodeLatencyChanged` 懒挂全局监听；后端全部探测写入点把被接受的结果推送出来，Dashboard 延迟卡/NodesPage/SimpleServers 订阅就地刷新，tested_at 防旧值回滚）、`deep-link-urls`（ImportIntentContext）、`core-download-progress`（SettingsPage）、`remote-rule-set-status` 与 `rule-set-apply-status`（RulesPage）。
- `types.ts` — 与 Rust `domain/*` 对应的手写类型（注意 `ProxyStatus`、`AppSettings`、`ManualNodeDraft` 46 字段等需两边同步）。

### 6.3 页面（pages/）

| 页面 | 要点 |
|---|---|
| `DashboardPage` (1399 行) | 启停/重启、capture/出站模式快控、节点选择、配置预览弹窗（按内核显示 JSON/YAML）、60 样本迷你图、LAN IP、版本（并行三内核 info）；「网络探测」卡（点击=刷新当前节点延迟+出口 IP 竞速探测，`running:nodeId` 边沿自动重探，见 §5.6 `services/exit_ip.rs`）；hero ⋯ 指定内核子菜单（三选项） |
| `ConfigPage` | 订阅卡片（流量配额条 + **配置类型标签** `.sub-type-tag`：clash_yaml→mihomo、singbox_json→sing-box、uri_list→URI；custom 档显示 custom_kind）、⋮ 菜单 **「查看配置」** 弹窗（`getSubscriptionRawConfig` 拉原始报文、可复制、无快照时提示刷新订阅）与 **「内核支持」** 弹窗（`getSubscriptionCoreSupport`：三内核支持数比例条 + 解析期跳过明细）、排他选择/Mix、`AddConfigModal`、深链预填（`useImportIntent`）；第三分组「sing-box 配置」更名 **「自定义配置」**（source_kind=`custom`，列出全部自定义配置并显示类型标签） |
| `NodesPage` (464) | 列表/网格（`useVirtualRange`×2）、搜索排序测速（批量测速按当前排序下发 ids，后端逐节点流式回传、rAF 合帧就地刷新，见 §7 改测速）、改名、custom 配置节点；切节点 `waitForCoreRestart` |
| `TrafficPage` (~70) | 三 tab 容器：实时连接 / 请求历史 / 失败请求；Xray 模式下三 tab 降级为空态，页首提示改指向「日志 → 内核日志」（原内嵌日志视图已移除，避免与 LogsPage 重复） |
| `ConnectionsPage` (215) | 1.5s revision-delta 增量轮询（`list_connection_changes` + `applyConnectionChanges`） |
| `RequestsPage` (258) / `FailuresPage` (510) | 已关闭请求/失败请求日志；Failures 可一键生成封锁规则集 |
| `LogsPage` (~380) | 四 tab：应用日志（1.2s 增量，级别过滤+搜索）/ sing-box / Xray / mihomo 内核日志（tab 带运行绿灯，原始 stdout，1.2s 轮询 `get_core_log_tail(kind)`；多核下主核/副进程各写各的 `logs/<prefix>-<hour>.log`，按 kind 读取，见 `Runtime::core_log_tail_for`）；头部 自动滚动/刷新/清空 与级别过滤、搜索框对所有 tab 统一（清空=app 清库 / kernel 截断当前小时文件 `clear_core_log`） |
| `SettingsPage` (1456) | 8 tab：app/ports/rules/chain/multiCore/dns/hosts/core；内嵌 Rules/Chain/多核模式/Dns/Hosts 页；三内核行（各自版本/下载/更新，进度事件按 kind 分流）、multiCore tab=**「内核设置」**（顶部「TLS 分片」双开关 sing-box/Xray——仅对应内核模式下生效、mihomo 无此能力无开关，见 §5.4；下方多核模式启停开关 + 协议→内核 table，§9.20；按协议支持面各选 Xray/mihomo——如 hy2 两者皆可，masque 仅 mihomo 且「auto」显示「未启用」而非「跟随主内核」，因 sing-box 无原生出路）、更新检查、诊断、托盘图标选择 |
| `RulesPage` (2145) | ★ 最大页面：规则集侧栏+编辑器、本地/远程集、策略/DNS 策略、route.final、拖拽排序、远程规则项浏览；geodata 内核（Xray/mihomo）下内置 3 条显示为 geodata 卡（来源/文件按内核区分，更新走 `refresh_geodata(kind)`），自建 .srs 置灰；策略可指向 chain（`chain_id`） |
| `ChainPage` (~1700) | 高密度管理列表，内嵌于 Settings：节点池=单容器紧凑行（名称+关键字+模式pill+计数+引用，行尾 `RowMenu` ⋮ 菜单，复用 rule-menu 范式），链卡=头行徽标（跳数/规则引用/⋮）+ 地铁线 stepper；链路编辑器为 xyflow（`@xyflow/react`）画布：侧栏候选拖入/点击追加（WKWebView 无 HTML5 DnD，用指针事件自实现）、`hopsFromGraph` 单线路径校验（连线时 `isValidConnection` 即时拦截分支/环/自环）、图序号徽标 + 实时有效性状态行、整理布局按钮；fitView 仅在打开已有链路且节点完成测量后执行一次（`useNodesInitialized`），否则画布会因未测量节点算出坏视口而看似空白、或投放后视口跳走 |
| `DnsPage` (~640) / `HostsPage` (463) | DNS/Hosts 管理，通常内嵌于 Settings；通用卡（劫持/兜底/缓存/FakeIP/远程加密 DNS——FakeIP 仅启用开关+⋯ 弹窗编辑池/IPv6/bypass，远程加密 DNS ⋯ 弹窗按行编辑 DoH 池，空=内置 1.1.1.1/8.8.8.8，见 §18①）；诊断 = 规则列表风格表格（域名|策略|匹配|DNS 服务器|内核解析，一行一域名，`diagnoseDns` 支持全部诊断与行内单个诊断，自定义域名 localStorage 持久化 `satelite.dnsDiagDomains`，本地/国内路径标红 ⚠ 泄露风险，见 `services/dns_diag.rs`） |

### 6.4 简洁模式（ui/simple/）

`SimpleShell`（4 tab：connect/servers/traffic/settings）+ 各页。复用玻璃设计语言与 `AddConfigModal`；`SimpleTrafficSpark` 为 SVG 迷你流量图。新增面向普通用户的轻量入口时改这里。

### 6.5 组件与 hooks

- 设计系统：`GlassButton`、`GlassSeg`（区分用户点击与状态重绘才做动画）、`GlassSwitch(+Control)`、`SolidSelect`（**自绘下拉：macOS WKWebView 原生 select 无法主题化**，SolidSelect.tsx:26 注释）。
- 首页视觉：`HeroVisual`（按 `heroStyle` 分发）→ `ParticleSphere`（three.js，lazy）/ `FaceMark`（Canvas2D 笑脸）/ 经典轨道。
- 弹窗：`AddConfigModal`（url/file/paste/手动节点/**自定义配置** 五种来源——第四 tab 原「自写配置」（sing-box 限定）2026-09-25 更名并支持 sing-box/mihomo/Xray 三类型：粘贴框下实时显示 `detectCustomKind` 识别结果（前端镜像后端 `detect_custom_config_kind`，仅提示、后端为准）、提交走 `add_subscription_custom`；重复 URL 仅信息提示不拦截——同 URL 可多订阅，`modal.duplicateUrlInfo`）、`EditLocalNodesModal`、`EditNodeModal`（节点卡片 ⋮ 菜单「编辑节点」：`get_node_draft` 预填 + `NodeDraftFields` 编辑 + 「订阅更新会覆盖编辑」警告条，保存走 `updateNode`→`update_node`；custom 运行时不显示）、`NodeDraftFields`（16 协议条件字段表单，与 `ManualNodeDraft` 对应）、`NodeDetailModal`（节点卡片 ⋮ 菜单「详情」只读弹窗：按协议渲染协议参数/TLS/传输层；数据来自 `list_all_nodes` 载荷里 serde-flatten 的完整 `ProxyNode`（`config/tls/transport`），前端 `types.ts` 已镜像为 `ProtocolConfig` tagged union，**纯前端无后端命令**）、`AccentColorPickerModal`（自定义主题色取色器；`theme/accents.ts` 支持 `#rrggbb` 自定义 accent，Rust `update_settings` 同步放行）。节点卡片附属信息徽标（`vless`+`reality`/`tls`/`ws`…）由 `nodeTooltip.ts::nodeFeatureBadges` 产出，NodesPage 行/卡与 SimpleServers 行共用（`.node-proto-feat`）；协议分组/搜索仍按主协议字符串。
- hooks：
  - `useVisibleInterval` — **通用轮询原语**：页面隐藏暂停、回调不重叠、可见即重发；
  - `useVirtualRange` — 基于 `.main` 滚动容器的列表虚拟化（支持网格 itemsPerRow）；
  - `useRulesetDragSort` — 手写指针拖拽排序（Tauri WebView 里 HTML5 DnD 不可靠，见文件头注释）：5px 阈值、LERP 跟随克隆、FLIP 动画、边缘自动滚动、Esc 中止；
  - `useCaptureModeSwitch` — 乐观切换 + 单飞排空队列（防内核并发切换报错）。

### 6.6 i18n / 主题 / 其他工具模块

- `i18n/messages.ts` — `en`（~760 键，`as const`）+ `zh: Record<MessageKey, string>`。**加文案必须两边同加，否则 TS 编译错**。键前缀：`common./nav./simple./dashboard./nodes./config./modal./nodeDraft./uiMenu./theme./crash./traffic./conn./logs./settings./rules./dns./hosts./failures.`；`translate()` 支持 `{n}` 插值。**2026-09-21 完成前端硬编码中文清零**：AddConfigModal（`modal.*`）与手动节点表单 NodeDraftFields（`nodeDraft.*`）曾是最大的两块硬编码区，均已接入 i18n；订阅到期文案的中文习语（长期有效/永久有效）在 en 下显示期映射（`ConfigPage::localizeExpireText`），托盘菜单走 Rust 侧双语（§5.7）；内置集名「系统 Hosts」由 HostsPage 按 id 用 `hosts.systemSetName` 覆盖显示（后端 store 里的 name 是持久化数据不改）。**已知残留**：后端错误消息（TUN 提权提示、导入/启动错误文本等）仍为中文直出，`DashboardPage::isTunPermissionError` 靠中文锚点匹配即此故。
- `theme/` — `ThemeId = "day"(浅,Rust `default_theme` 默认) | "aerospace"(深)`；theme/uiMode/heroStyle 三者均镜像到 localStorage（`index.html` 内联脚本 + Provider 初始 `useState` 同步读取）防 WebView 重建首帧闪烁/误挂 three.js hero；`accents.ts` 6 个主题色，由一个基色派生整个 `--primary*` 变量族（Rec.709 亮度决定 `--on-primary`）。语义色 `--success*` 为固定绿（App.css tokens），**不随主题色**（ok/直连/测速良好语义稳定）；自定义 `#rrggbb` accent 在 `applyAccentToDom` 应用时按主题做亮度钳制（深色提亮 ≥0.5 / 浅色加深 ≤0.6）保证文字对比度，存储仍保留原始 hex。另有独立背景光晕色 `glow_color`（`"accent"`=跟随主题色 / 预设 id / `#rrggbb`），`applyGlowToDom` 下发 `--glow-rgb`（原始色，驱动 `--hero-glow`）与 `--glow-deep-rgb`（按感知亮度归一化的深色变体，驱动 app-shell 大气层，防止亮色光晕把暗色主题洗亮）。
- 独立模块：`customNodes.ts`（custom 节点客户端侧过滤/排序/分页镜像）、`latencyStream.ts`（批量测速流式结果 rAF 合帧缓冲，Nodes/SimpleServers 共用）、`subscriptionUrl.ts`（URL 规范化去重）、`deepLink.ts`（深链解析→ImportPrefill）、`coreBusy.ts`（全局 busy 深度计数 + `waitForCoreRestart`）、`connectionChanges.ts`（delta 合并纯函数）、`trafficFilter.ts`（all/direct/proxy 分类）、`coreLog.ts`（内核原始日志行级别推断，LogsPage 内核日志视图使用）、`windowLayout.ts`（窗口尺寸/模式）。
- `App.css` — 单文件 ~7.6k 行，按 `/* —— 段落 —— */` 横幅分节（tokens → shell → topnav → page → nodes → …）；玻璃材质 = 半透明 rgba + `backdrop-filter` + 左上光源 `::after`；专业窗口固定 960px 宽（网格断点据此调）。

## 7. 常见修改场景 → 去哪里改

| 需求 | 位置 |
|---|---|
| 新增设置项 | `domain/settings.rs`（`AppSettings`）→ `storage/store.rs`（迁移如需）→ `config/builder.rs` **和/或 `config/xray.rs` / `config/mihomo.rs`**（生成如需，多内核都要考虑）→ `src/types.ts`（`AppSettings`）→ 页面 UI + `i18n/messages.ts` 双语 |
| 新增 command | `src-tauri/src/commands/<域>.rs` → `commands/mod.rs` re-export → `lib.rs` `generate_handler![]` 注册 → `src/api.ts` 加封装 |
| 新增订阅格式/协议解析 | `src-tauri/src/subscription/`（clash/singbox/uri/manual）+ `domain/node.rs`（新协议记得看 `Protocol::xray_supported`/`mihomo_supported` 与 `supports_node`） |
| 改 sing-box 配置生成 | `config/builder.rs`（路由/inbound/outbound）、`config/dns_build.rs`（DNS） |
| 改 Xray 配置生成 | `config/xray.rs`（改动后用 `xray run -test -c` 手工验证，失败退出码 23；副进程配置 `build_xray_sidecar_config` 有专属 live 测试） |
| 改 mihomo 配置生成 | `config/mihomo.rs`（Clash YAML；改动后跑单测 + `cargo test --lib config::mihomo::tests::live_config_validates -- --ignored` 用真 mihomo `-t` 验证） |
| 改副进程委托（多核模式） | `runtime.rs::compute_sidecar_plan`（计划，按 kind 分组）+ `config/builder.rs`（`BuildOptions.sidecar`/`SidecarPlan`/`SidecarPort`）+ `config/xray.rs::build_xray_sidecar_config` / `config/mihomo.rs::build_mihomo_sidecar_config`（两副配置）+ `config/write.rs`（`xray-sidecar.json` / `mihomo-sidecar.yaml`）+ 设置项 `multi_core_*`/`protocol_cores`（`commands/config.rs::update_settings`，core 值限 xray/mihomo）+ UI `SettingsPage` 多核模式 tab；详见 §9.20 |
| 改规则集逻辑 | `domain/rule.rs`（模型）+ `config/builder.rs`（sing-box 编译）+ `config/xray.rs`（Xray 映射）+ `config/mihomo.rs`（mihomo 映射）+ `commands/rules.rs` + `src/pages/RulesPage.tsx` |
| 改内核启动参数/生命周期 | `core/manager.rs` + `core/kind.rs`（kind 相关差异集中在 kind.rs） |
| 加文案 | `src/i18n/messages.ts` 的 `en` 和 `zh` **都要加** |
| 加页面 | `src/pages/` + `App.tsx` lazy 导入 + `NavKey`（types.ts）+ `TopNav` + i18n `nav.*` |
| 改样式 | `src/App.css` 对应段落；新主题色变体在 `theme/accents.ts` |
| 加托盘功能 | `src-tauri/src/tray.rs` |
| 改测速 | `services/latency.rs` + `src/api.ts`：节点页「测真实延迟」= `test_nodes_latency`（内核运行时走 Clash delay API 经真实代理链路，unified delay 双探测取第二次）；「Ping 测试」= `ping_nodes_latency`（纯 TCP 直连、并发 30、内核运行时也不经内核；QUIC-only 协议报 unsupported）；智能选路的排序预筛 = `probe_nodes_ranked`（TCP ping + QUIC-only 内核兜底，见 §5.1）。TCP 直连只反映可达性，会漏报 REALITY/Vision 这类「TCP 活但代理死」的节点——故智能切换的出口巡检与候选验证一律用内核 URL 探测。**三个批量测速 command 均带必填 `on_result: Channel<LatencyResult>`**（api.ts 用 `@tauri-apps/api/core` 的 `Channel` 封装，调用方没有回调也给静默 channel）：后端每个探测完成即推送，前端（NodesPage/SimpleServersPage）按 rAF 合帧逐节点就地刷新；前端 ids 经 `list_node_ids(query, sort_mode)` 按当前显示排序下发，后端 `load_nodes_in_display_order` 保持该顺序起测；手动测速一律不读缓存（`use_cache=false`，结果仍写回），共享探测缓存 TTL 成功 30s / 失败 15s（2026-08）。**落库与共享（2026-09-16）**：全部延迟写入走 `AppStore::update_node_latency(id, ms, at, method)` 唯一入口，带真实延迟优先规则（`clash_api` 恒覆盖；`tcp` ping 不得覆盖已有真实值——含失败的真实值，防「TCP 活但代理死」假阳性；`unsupported`/`error` 不落库），口径存 `StoredNode.latency_method`；命令侧 `AppState::apply_latency_results`（阻塞）、引擎侧 `try_apply_latency_results`（try-lock，§9.24）把被接受的结果经 `node-latency-changed` 事件推送前端（`api.ts::onNodeLatencyChanged`），Dashboard 延迟卡/NodesPage/SimpleServers 行内实时刷新 |
| 改内核下载/资产 | `core/download.rs` + `core/assets.rs` + `scripts/fetch-bundled-*-<平台>` 脚本 + `tauri.*.conf.json` resources 四处联动；给内核新增运行时依赖时记得挂进 `assets.rs::prefetch_runtime_assets`（下载后即时预取，§5.5/§9.22） |
| 打 Windows 便携版 | `scripts/build-windows.ps1 -Bundle portable`（zip 组装逻辑在此脚本；Rust 侧便携行为集中在 `src-tauri/src/portable.rs`，见 §9.19） |
| 重大架构 / 模块 / 流程变动 | **同步更新本文档对应章节**（规则见 §0） |

## 8. 构建细节与产物

- **版本号**：`package.json`（1.0.24）是唯一真源，`tauri.conf.json` 引用它；`Cargo.toml`（1.0.4）落后且不自动同步——发版时手动检查三处。**打 tag / 发版前必须先把 `package.json` 的 `version` 改成与 tag 一致**（如 tag `v1.0.18` → `version: "1.0.18"`），否则 `tauri.conf.json` 引用的版本号不匹配会导致构建失败。
- **产物路径**：DMG → `src-tauri/target/<aarch64|x86_64>-apple-darwin/release/bundle/dmg/`；Windows → `src-tauri/target/release/bundle/nsis/`（或 `.../msi/`）。
- **Rust 测试布局**：集成测试 `src-tauri/tests/parse_subscription.rs`（fixtures 在 `tests/fixtures/`：clash yaml ×2、singbox json ×1）；`download_core_live.rs` 为 `#[ignore]` 真网测试；单测散落各文件 `#[cfg(test)]`。**真内核 live 验证**（`#[ignore]`，需先跑 `scripts/fetch-bundled-xray-*` 取得 dev 树二进制，统一入口 `cargo test --lib config::xray::tests::live_ -- --ignored`）：`live_share_link_pipeline_validates`（真实形状分享链接→解析→生成→`xray run -test`，含 edgetunnel vless+xhttp）与 `live_all_protocol_matrix_validates`（全协议×传输×TLS 矩阵 + 三类预期表 Valid/GenReject/CoreReject——CoreReject 钉住「v26 已移除 h2/http 传输」等已知内核拒绝项，内核行为变化时表会翻转报警）；mihomo 同款见 `config/mihomo.rs`。
- **换行符**：`.gitattributes` 规定源码 eol=lf、`.ps1/.bat/.cmd` 为 CRLF。
- **内核版本**：sing-box 各平台统一 v1.13.18（`scripts/fetch-bundled-core-*` + `core/kind.rs::fallback_version` 两处同步）；Xray 各平台统一 v26.3.27（`scripts/fetch-bundled-xray-*` + `core/kind.rs::fallback_version` 两处同步）；mihomo 各平台统一 v1.19.30（`scripts/fetch-bundled-mihomo-*` + `core/kind.rs::fallback_version` 两处同步）；升级时三处（各平台脚本 + kind.rs）联动改，缺一处会导致运行时兜底版本与安装包内实际版本不一致。

## 9. 约定与坑（agent 必读）

1. **Clash API 客户端禁用 `reqwest::blocking`** — 嵌套 Tokio runtime 会在 Tauri async worker panic；用 `ureq`（`api/clash_api.rs` 文件头有说明）。reqwest 仅用于异步下载内核。
2. **`resources/bin/**/sing-box*`、`xray*`、`mihomo*`、`*.dat`、`wintun.dll`、`libcronet.dll`、`resources/rule-sets/*.srs`、`mihomo-geodata/` 不入库** — 本地没有属正常，dev 首次运行自动下载。
3. **`BUILTIN_REMOTE_RULE_SETS`（`domain/rule.rs`）与 `scripts/fetch-bundled-rule-sets.sh` 必须同步**；内置 3 条的 Xray geosite 映射在 `config/xray.rs`（`builtin_remote_xray_rule` + DNS 分类处）、mihomo 映射在 `config/mihomo.rs`（`builtin_remote_mihomo_rule` + DNS 分类处），改 id 时多处联动。
4. **i18n 双语强约束** — `messages.ts` 中 `zh` 的类型是 `Record<MessageKey, string>`，漏键编译失败。
5. **前端↔后端类型手工同步** — `src/types.ts` 与 `domain/*` 无代码生成；改 Rust 序列化结构记得同步 TS（部分 invoke 同时发 camelCase+snake_case 参数以兼容，见 `api.ts`）。
6. **单窗口** — 无多窗口 API 用法；窗口可调性（2026-09）：pro 可缩放但下限=设计尺寸 960×720（zoom 只放大不缩小），simple 下限 320×480；两者均可自由放大、无上限，UI 经 `useViewportScale` 等比缩放（拖拽实时生效，首次应用不播过渡动画）。尺寸持久化**双轨**：前端 localStorage（模式切换恢复，`watchWindowSize` 读 Tauri `innerSize` 而非 `innerWidth`——zoom 会扭曲 DOM 测量值）+ Rust 侧 `data/window_size_<mode>`（**2026-09-24 起格式扩为 `"<w> <h> <x> <y>"` 含窗口位置**，旧两段式文件向后兼容；隐藏进托盘/退出时经 `persist_main_window_layout` 捕获、最大化跳过；托盘重建 `show_main` 与冷启动 `restore_main_window_layout` 据此让窗口**出生即目标尺寸与位置**——重建窗口若不显式给位置，Windows 会 CW_USEDEFAULT 级联摆放，每次托盘唤醒位置都漂移（2026-09-24 修复的 bug）；位置经 `position_reachable` 对各显示器工作区校验（拔掉显示器/分辨率变化后落屏外则改回居中），无记住位置亦居中）。
7. **平台差异** — 系统代理 `proxy/windows.rs|macos.rs`（Linux 用 stub）、TUN 提权 `core/elevate.rs`（Win）与 `core/macos_auth.rs`、进程绑定 `core/job.rs`（仅 Win）。改平台行为时注意 cfg 分支。
8. **HTML5 拖拽在 Tauri WebView 不可靠** — 排序一律用 `useRulesetDragSort` 模式（指针事件手写）。
9. **页面切换 = 重挂载**（`key={nav}`）— 页面自身状态不跨切换保留；跨页面共享靠 `api.ts` 模块级快照（peek/keep）。
10. **规则变更应用是防抖+串行**（`rule_apply.rs` 500ms 合并）— UI 事件 `rule-set-apply-status` 回报结果，不要假设保存即重启完成。
11. **store.json 解析失败会拒启**（防覆盖用户新 schema 数据）；未知字段保留在 `retained_*` 写回。改存储结构时保持向后兼容 + `schema_version` 迁移。
12. **窗口关闭默认进托盘**；真正退出需 `exit_allowed`（`state.is_exit_allowed()`），退出时 `shutdown_runtime()` 停内核清代理。
13. **三内核配置生成相互独立** — `config/builder.rs`（sing-box）/ `config/xray.rs` / `config/mihomo.rs` 不共享生成代码，只共享 domain 模型与 `BuildOptions`；改路由/协议/DNS 语义时**三边都要改**并各跑单测。
14. **Xray 无 Clash API** — 无逐连接数据/热切节点/delay API：切节点与规则变更=重启进程（`select_current_node_serialized` 返回 restart_needed）；连接三页面在 Xray 下为空态；smart_switch 禁用。kernel 自动选路的首页节点同步走 `XrayMetrics::dominant_outbound_tag`（Xray 无选点 API——用 `/debug/vars` 的逐 outbound 计数器增量推断 balancer 当前选中的节点，空闲轮询保持上次选择）。流量统计靠 metrics `/debug/vars`（`api/xray_metrics.rs`）。Xray/mihomo 模式下节点列表**后端过滤**不支持协议的节点（`list_all_nodes`/`list_nodes_page`/`list_node_ids`，协议判定在 `Protocol::xray_supported`/`mihomo_supported`，节点级判定（Xray 的 REALITY 传输组合、mihomo 的 ss+shadow-tls）统一在 `CoreKind::supports_node`），切回 sing-box 即恢复显示；首页"指定配置"的自写 sing-box 配置项在 Xray/mihomo 下置灰。
15. **Xray 资产依赖** — `geosite:`/`geoip:`（含 `geoip:private`）需要 geosite.dat/geoip.dat（bundled 或运行时下载，`core/assets.rs::ensure_geodata`）；Windows TUN 需要 wintun.dll（Xray zip 不带）。缺资产时 Xray 启动会失败，报错要可读。
16. **`.srs` 规则集是 sing-box 专有** — Xray/mihomo 生成器跳过用户自建远程 `.srs` 集（内置 3 条走 geodata 映射）；`srs.rs` decompile 固定用 sing-box 二进制。Xray/mihomo 模式下 RulesPage 的内置 3 条显示为 geodata 来源卡（Xray：matcher + Loyalsoldier dat；mihomo：MetaCubeX mmdb/GeoSite.dat），"更新"走 `refresh_geodata(kind)` 重下 geodata 而非 `.srs`。
17. **mihomo 特有约定** — ① Clash YAML 配置写 `config/active.yaml`（JSON 系共用 active.json，互不混用）；启动参数 `-f <abs> -d <data>/mihomo`（config 必须绝对路径）。② geodata 在 `<data>/mihomo/`：`Country.mmdb`（MaxMind）+ `GeoSite.dat`（MetaCubeX mrs，**精确大小写**）——与 Xray 的 bin/geosite.dat 同名不同格式绝不能共目录；缺失时 mihomo 自带的下载会经由未启动的代理 dial 而超时，GEOIP/GEOSITE 规则直接让内核退出，故启动前必须 `ensure_mihomo_geodata`（下载内核时已由 `prefetch_runtime_assets` 预取，此处为兜底）。③ 协议面：mihomo（标准 Clash Meta + uTLS）支持 REALITY/Vision 全组合与全部 vmess 传输，仅 Naive/Tor/独立 ShadowTls 与 ss+shadow-tls 组合被 `supports_node` 过滤。④ `find-process-mode` 真实生效，已接 `AppSettings.find_process`（strict/off）。⑤ DNS 支持 `system` 解析器（Local 分类与 dns_final=local 直用）；Windows TUN 用 `bin/wintun.dll`（与 Xray 共用）。⑥ Clash API 全兼容：热切节点/连接监控/delay 测速/智能切换与 sing-box 同款复用（组名恒 `proxy`，kernel 模式它就是 url-test 组）。注意 mihomo 的 `/connections` chains 是完整 `[节点, 组]`（state 里的 "proxy"→当前节点名解析对 mihomo 无害）。⑦ **amd64 发行包一律用 `-compatible` 变体（GOAMD64=v1，2026-09）**：mihomo 无后缀的默认 amd64 包按 GOAMD64=v3 编译（需 AVX2/BMI2），在 Rosetta 2（Intel 版应用跑在 Apple Silicon 上）与 2013 年前后无 AVX2 的老 Intel CPU 上 `-v` 自检直接 fatal「This program can only be run on AMD64 processors with v3 microarchitecture support」——下载安装报 `version exit exit status: 1` 即此症状（sing-box/Xray 的 amd64 包是 v1 基线，Rosetta 下能跑，故三内核只有 mihomo 中招）。代理内核加解密热点走 AES-NI（v1 即有），v3 无实际收益。资产名生成单点在 `CoreKind::asset_name`（amd64 → `mihomo-{plat}-compatible-v{ver}`，应用内下载/synthetic fallback URL 均经它），fetch 脚本同名约定；Windows compatible zip 内部 exe 为 `mihomo-windows-amd64-compatible.exe`，`download.rs` 前缀匹配器与 ps1 通配符均兼容。
18. **三内核 DNS 语义对照（改 DNS 时逐项核对）** — 池与兜底已统一（2026-08 重构；2026-09 远程池开放用户自定义）：① **共享解析池**：`domain/dns.rs` 的 `REMOTE_DNS_POOL`（1.1.1.1+8.8.8.8 DoH）/ `DOMESTIC_DNS_POOL`（223.5.5.5+119.29.29.29 明文 UDP）是三生成器唯一真源，切内核不换解析服务器；**远程池可被用户覆盖**：`DnsSettings.remote_dns`（DNS 页「远程加密 DNS」⋯ 弹窗编辑，仅收 `https://` DoH，trim/去重/上限 8 条，`update_dns_settings` 里 `normalize_remote_dns`+`validate_remote_dns` 把关）非空即整体替换内置池——三生成器与 `dns_diag` 路径推演一律读 `effective_remote_pool()`，**绝不直接引用常量或裸字段**（保证自定义池一处生效处处生效）；发射按各内核原生能力：mihomo 并发竞速整池、Xray 池内顺序回退（第二远程条目无 domains=纯池内备援，**单条自定义池无备援条目**）、sing-box 规则只指向单 tag（无竞速）故只发 pool[0]（自定义 URL 的端口/路径拆进 `server_port`/`path`，`dns_build.rs::doh_parts`；域名型端点由 `route.default_domain_resolver` 引导解析）。② **`dns_final`（DNS 页「默认解析」）是唯一兜底**：mihomo `nameserver`=final 池、**不写 `fallback` 槽**（mihomo fallback-filter 语义会对未分类境外域名发明文直连查询、且偏好其答案——曾构成 DNS 泄露）；Xray 主服务器（index 0）=final 池、其余池全部 `skipFallback`（只应答自己的分类域名，无跨池回落，**永不追加 localhost 系统解析**——原 `leak_protect` 开关与 Xray 的 localhost 逃生通道已删除，2026-08）；sing-box `dns.final`=final 池（本就无跨池机制）。③ 远程 DNS 经代理出站：三内核均为 **DoH over TCP 经代理**（sing-box `detour:"proxy"` / Xray dns-module 经主出站 / mihomo `#proxy` 尾缀），**自定义池同样强制经代理出站**。**Xray 勿改回明文 UDP**——UDP-less 节点（socks5 无 UDP ASSOCIATE，如 ssh -D）会让远程 DNS 必挂、域名被透传给出口侧解析，测试站判为 DNS 泄露（实战事故）。Direct 出站模式例外，均直连。④ 节点域名解析：sing-box 用 `route.default_domain_resolver`（TUN 下=国内明文，非 TUN=系统）；mihomo 用 `proxy-server-nameserver`（国内明文池）；Xray 无等价物但实测未复现问题，出现「切 Xray 后节点解析失败」再加固。
19. **Windows 便携版约定（`src-tauri/src/portable.rs`）** — exe 旁存在 `portable.flag` 即便携模式：**exe 目录 = 数据根**（`data/`、`config/`、`bin/`、`logs/`、`mihomo/`、`remote-rule-sets/`、`webview/` 全在 exe 旁，不写 AppData）。① **禁止直连 `app.path().app_data_dir()`**——新增代码一律走 `portable::resolve_app_data_dir(&app)`（存量 4 处泄漏已收敛：`remote_rule_auto.rs`×2、`commands/rules.rs`、`lib.rs::set_ui_mode_pref`）；`AppState.app_data_dir` 锚点在 `lib.rs` setup 早已走便携覆盖。② **WebView2 用户目录必须在两条创建路径同时重定向**：配置窗口经 `portable::patch_context`（启动前把 `windows[].create` 置 false，setup 里 `build_main_window` 用 `.data_directory()` 重建——Tauri 配置窗口先于 setup 创建、且 conf 的 `dataDirectory` 只能锚定在 `%LOCALAPPDATA%`，无法走配置）；托盘重建窗口在 `window_ctrl::show_main`。漏一边会出现双 WebView 档案。③ `resource_dir()` Windows 上恒为 exe 目录，便携 zip 的 `resources/` 布局与安装版一致，`core/paths.rs` 候选链零改动。④ 便携与安装版共用 identifier → single-instance 互斥，不可同时运行；深链 HKCU 每次启动用 `current_exe` 重写（移动目录自愈），开机自启动 Run 键是绝对路径（移动目录后需重开）。⑤ zip 组装在 `build-windows.ps1 -Bundle portable`：`tauri build --no-bundle` 出 exe，resources **按当前生效 conf 的 `bundle.resources` 清单自拷**（与安装包内容自同步，勿在脚本里硬编码文件列表）。
20. **副进程委托 / 多核模式（`settings.multi_core_enabled` + `protocol_cores`，2026-08；2026-09 泛化为双副进程并接入 masque）** — sing-box 固定主监听（mixed/TUN/Clash API/诊断全在主核），多核 tab（Settings「多核模式」，位于代理链之后）里把协议钉到 Xray 或 mihomo 的，其节点经 loopback socks 交给对应副进程转发（**三核并存**：vmess→Xray + masque→mihomo 可同时委托）。① **仅生成配置 + `core_type=singbox` 模式生效**；`set_core_type` 切到其他内核时经 `enforce_multi_core_scope()` **自动关闭**（`protocol_cores` 钉选保留，切回 sing-box 重新打开开关即恢复）；`update_settings` 拒绝在非 sing-box 主核下启用（清洗器只放行 `core=="xray"|"mihomo"`）；mihomo/Xray 主模式与 custom 配置一律无副进程。② **tag 不变是设计核心**：委托节点在 sing-box 侧仍是 `node-<id16>` 只是 `type:"socks"` 指向 `127.0.0.1:<基址+i>`——selector/规则钉定/smart 池/Clash 热切/delay 测速零改动照常工作；改这里务必保住该不变量。③ 两副进程每节点一个独立 loopback mixed 入站（`in-sc-<port>`）：Xray 路由按 `inboundTag→outboundTag` 1:1 派发，mihomo 用 listener `proxy:` 字段按监听器钉出站（等价语义，`udp: true`）；**端口为单一连续索引空间**（base+i 对全部候选混排，两副进程不可能撞端口）；上限 1024 个（`SIDECAR_MAX_NODES`），超限/目标内核 `supports_node` 不过（如 REALITY+ws 给 Xray）/被 chain hop 直连钉定/WireGuard/候选端口撞上保留集（mixed/api/extra/DIAG 26486，命中即跳过且**索引照常推进**）的节点**自动回退原生出站**并记告警——masque 回退原生=被过滤（sing-box 无 masque 出站，见 §9.21），日志带「钉到 mihomo 或切内核」指引。④ 启动序=主核 health OK → 按计划中出现的 kind 逐个写副配置（`xray-sidecar.json` / `mihomo-sidecar.yaml`，绝不写 active.\*）→ 起副进程；计划非空时**启动前仅预检对应内核二进制存在**（缺失必须前置报错而非半启动）；副端口**不做逐端口 lsof 预检**，残留占用交给 `start_with_ports` 内的 `ensure_ports_free` 清理，真实 bind 冲突以副进程 FATAL 浮出→整体回滚（停主核+已启动的另一副进程）。停/重启/退出先停全部副进程；watchdog 对每个副进程独立预算（`SidecarWatch`）。⑤ 两份副配置刻意无 DNS/geodata/api/metrics/tun——mihomo 副进程 `-t` 不建网卡可正常预校验、无需 `ensure_mihomo_geodata`；live 验证跑 `cargo test --lib config::xray::tests::live_sidecar_config_validates -- --ignored` 与 `config::mihomo::tests::live_mihomo_sidecar_config_validates -- --ignored`。`protocol_cores` 只存实际委托行（`core=="xray"|"mihomo"`），未列出的协议跟随主内核；**UI 按协议支持面开放目标**——双方都支持的协议（vmess/vless/ss/trojan/hy2/socks5/http）可选 Xray 或 mihomo（如 hy2 钉 mihomo 即经 mihomo 副进程转发，与 masque 同路），masque 仅 mihomo 且其「auto」显示「未启用」（未钉选=节点被过滤，无原生出路）；WireGuard 行维持 Xray 标签以匹配计划行为（endpoint 形态、`compute_sidecar_plan` 一律排除委托）。⑥ **TUN 回环防护（2026-09-23）**：副进程自身出站是普通 socket，不享受 sing-box 的进程级 TUN 绕过（`route.auto_detect_interface` 只保护 sing-box 自己的出站）；TUN 下 auto_route 把副进程拨真实节点服务器的流量抓回 tun-in、按 final=proxy 路由到委托节点自己 → 无限回环。`build_singbox_config` 经 `tun_sidecar_guard_needed`（tun_enabled 且 plan 非空）在 sniff 之后、hijack-dns/block_quic/用户规则之前注入 `{process_name:[xray,xray.exe,mihomo,mihomo.exe]→direct}`（名字来自 `sidecar_process_names` 按 `used_kinds` 去重；sing-box 匹配=`filepath.Base(exe路径)` 的**大小写敏感 map 精确查找**，双平台变体都发保配置平台无关——应用自己落盘/拉起副进程二进制、全小写，大小写无实际风险；**配置里存在 process 规则即自动启用进程查找**（sing-box router.go `needFindProcess`），不依赖 `find_process` 设置；macOS 经 sysctl pcblist+proc_pidpath 原生支持，但其结构体大小按 `kern.osrelease` 硬编码，未来 macOS ABI 变动会静默失效——故有兜底）。**排序语义**：必须在 hijack-dns 前（副进程 UDP:53 走直连，避开「经远程 DoH 解析节点域名→DoH 走代理→代理恰需该域名」循环依赖）、必须在 block_quic 前（钉给 mihomo 的 hy2/tuic 是真 QUIC 出站，不能被误杀）。兜底：IP-literal 服务器的委托节点另发 `{ip_cidr:[<server>/32|/128]→direct}`（进程查找失效时防回环；sing-box 规则无法只圈「副进程的连接」，其他流量到该 IP 也直连——专用 IP 可接受，域名型/CDN 服务器只能靠 process 规则）。无 TUN 时不发（系统代理模式下副进程出站根本不经过 sing-box，process 规则也只在 TUN 连接上可解析）。
21. **未知传输 = 解析期跳过，绝不静默降级；xhttp 是唯一例外（2026-08）** — `Transport` 枚举 = Tcp/Ws/Grpc/Http/HttpUpgrade/**Xhttp{path,host,mode,extra}**；kcp/splithttp(=xhttp 别名) 之外的未知 `network`/`type` 一律报错 → 节点进 `skipped`（原因 `unsupported transport: <name>`）。曾因 `_ => Some(Transport::Tcp)` 兜底，vless+xhttp 被静默降级成 vless+tcp——能进库能生成但永远连不上。**xhttp 的内核语义**：Xray 独有（`streamSettings.network="xhttp"` + `xhttpSettings{path,host,mode,extra}`，mode 缺省 auto；**REALITY+xhttp 合法**；TLS 下 auto 归一为 packet-up=CDN 兼容档，无 alpn 时内核默认注入 `["h2","http/1.1"]`，均经 v26.3.27 源码核实）；`extra` 为 XHTTP 调优 JSON 对象——分享链 `extra=` 按 v2rayN 惯例 base64url 解码（裸 `{...}` 也收），clash `xhttp-opts.extra` 为裸 JSON，解析时必须是合法 JSON 对象否则丢弃（Xray 对坏 extra 整配置拒载，生成器二次校验兜底）。sing-box 无此传输——`node_to_outbound_tagged` 遇 Xhttp 节点报错（带过滤提示），该节点**从本次生成的配置中被过滤**并逐节点记 warn（`build_singbox_config` 的 errors 留痕），节点仍保留在库与列表中（`SingBox.supports_node` 不按传输拒绝）；mihomo 经 `Mihomo.supports_node` 过滤。**h2/http 传输与 xhttp 相反方向：Xray v26 已在配置层移除**（`network:"http"` 拒载），Xray 生成期报错跳过 + `CoreKind::supports_node(Xray)` 列表过滤，sing-box/mihomo 原生可用。**委托与否完全跟随用户的多核协议钉选**——`compute_sidecar_plan` 无 per-transport 特例，vless 钉到 Xray 时 xhttp vless 节点随协议委托，未钉则在纯 sing-box 配置中被过滤。总模型：**导入的节点全部入库保留，生成配置时按当前内核情况过滤不支持的**（masque 协议在 sing-box 下同款：`node_to_outbound_tagged` 报错过滤并指引钉到 mihomo 副进程/切内核，见 §9.20③；sing-box 列表层保持可见——`CoreKind::supports(SingBox)` 恒 true 是有意的，否则用户看不到要钉选什么）。解析面三处（uri.rs vless/vmess/trojan、clash.rs `network: xhttp|splithttp`+xhttp-opts、manual 表单）都收 xhttp（manual 表单无 mode/extra 字段，编辑会回落默认值）；sing-box 自写配置导入不含它。给新传输加解析时同步这些 match。`skip-cert-verify` 在 Xray 侧强制忽略（Xray ≥26 无 allowInsecure，输出即拒载），委托节点证书无效会连不上，属预期。
22. **命令线程模型：主线程禁阻塞（2026-08 卡死事故修复）** — Tauri 2 同步命令跑在主线程（官方文档："Commands without the async keyword are executed on the main thread"），主线程被阻塞 = 事件循环停转 = 窗口「未响应」。而 `state.rs::restart_proxy` 在**整个内核启动期间持有 store+runtime 两把锁**（含 mihomo geodata 首次下载、`-t` 预检、6–10s 健康等待）。实战事故：切 mihomo（首次缺 geodata 直连 GitHub 慢下载）时，LogsPage 每 1.2s 轮询的 `get_core_log_tail` 在主线程阻塞等 `lock_runtime()`，整个 UI 卡死到启动结束。**约定**：① 凡碰 store/runtime 锁或做文件/网络 IO 的 command 一律标 `#[tauri::command(async)]`（同步函数体改跑异步运行时，不占主线程；已转 52 处）；该属性要求返回值是 `Result`（宏编译期检查），非 Result 的命令（`get_lan_ip`/`diagnose_network`）必须保持极轻。② `get_proxy_status` 的 try_lock+缓存回退是另一套防护样板，继续保留。③ 新增 command 默认加 `(async)`；重型异步命令（`set_core_type`/`start_proxy` 等）沿用显式 `async_runtime::spawn_blocking` 模式。④ mihomo geodata 下载超时 30s（曾 120s×2 文件放大持锁窗口，`core/assets.rs`）。⑤ **运行时依赖在内核下载阶段即预取**：`download_core` 安装成功后调 `assets::prefetch_runtime_assets`（Xray→geodata、mihomo→geodata、两者 Windows→wintun；走 `current_download_proxy` 同款代理，失败仅 app_log warn，进度事件 stage=`assets` 前端显示「正在获取运行时资源」），启动时的 `ensure_*` 保留为兜底（bundled/存量安装用户不走下载路径）。
23. **Linux WebKitGTK 渲染器兜底（2026-09 Ubuntu 26.04/Wayland 闪退修复）** — WebKitGTK ≥2.42 的 DMA-BUF 渲染器在部分 Wayland/Mesa/NVIDIA 栈上创建 surfaceless EGL display 得到 `EGL_BAD_ALLOC` 并在 Web 进程直接 abort（应用闪退、stderr 仅剩 `Could not create surfaceless EGL display: EGL_BAD_ALLOC. Aborting...` 一行）。`main.rs::apply_webkitgtk_workarounds` 在一切 GTK/WebKit 初始化**之前**默认设 `WEBKIT_DISABLE_DMABUF_RENDERER=1`（用户显式设置过则不覆盖）；仍闪退的用户可再加 `WEBKIT_DISABLE_COMPOSITING_MODE=1`（强制软渲染）。启动日志里 `libgvfscommon.so: undefined symbol` / `Failed to load module: libgvfsdbus.so` 是 AppImage 打包的 GLib 旧于系统 gvfs 模块所致的**无害警告**（GIO 跳过该模块继续跑），不要误判为崩溃原因。
24. **后台引擎禁止同步阻塞在 async 任务里；超时救不了 mid-poll 阻塞（2026-09-13 智能切换引擎三连冻结事故）** — 事故：smart_switch 引擎三次在同一位置静默死亡（最后一条日志=巡检 trace，此后任何级别日志消失、死出口永不切换），期间 store 锁、内核、UI 全部正常。完整机理有两层：① `app_log::push` 曾在**调用者线程**同步 `eprintln!` 镜像 stderr——Windows ConHost QuickEdit（在控制台点击/选中文字）会暂停管道输出，写满后 `eprintln!` 永久阻塞，冻住恰好执行到日志行的任务；② **tokio 超时只在 await 点生效**——future 在 poll 中途阻塞于同步调用（等锁/挂死的 syscall）时会占死所在 worker，`timeout(...)` 包同一个 future **根本不会触发**（wrapper 无法被重新 poll），因此「加了硬超时」对这类卡死是无效的。**修复与约定（四层防护，缺一不可）**：① stderr 镜像只存在于日志 writer 线程（`WriterMessage::Entry` 分支内、落盘与 ack 之后；ack 先于镜像，调用方最多等自身 `PERSIST_ACK_TIMEOUT`）；② **每轮引擎工作 spawn 为独立子任务再由监督者 await**（`smart_switch::supervise_round`，用 `tokio::spawn` 以便保留句柄）——子任务 mid-poll 阻塞只搁浅子任务自身，监督者的 timer 在别的 worker 上照常触发；超时后 **`handle.abort()`**（在 await 点挂着的子任务可被取消，同步阻塞中的不可取消但也不再无感堆积），放弃该轮并 warn，循环永续；panic 也经 JoinError 浮出；轮次带 `tick timing: gates/patrol/…` 分段计时与 `probe_coalesced` 的 `slow probe: key_wait/permit_wait/probe` 排队诊断（>5s 才记）——下次卡轮可直接定位耗时层；③ **引擎热路径一律 try-lock**（`AppState::try_lock_store/try_lock_runtime/try_with_store_mut`）：锁竞争跳过本轮（60s 限流日志 `round skipped: … lock busy`）而不是排队冻死 worker——卡死的锁持有者最多让引擎降级，不会杀死引擎；锁 guard 一律块作用域包裹（let-else + drop 的写法会让 rustc 认为 guard 跨 await 存活，future !Send 编译失败）；④ **空转原因限流可见**（`engine idle: core transitioning / core not running`，60s 一次）——悬挂的 core_transitioning 标志（守卫持有者阻塞即悬挂）从此与「引擎死了」可区分；`begin_core_transition`/guard Drop 各有一条 debug 日志可追谁拿了没放。新代码要打日志一律走 `app_log::*`，**绝不**在后台任务里直接 eprintln/print；后台循环里的同步锁/IO 要么 try-lock+跳过、要么整体 spawn 成被监督的子任务。
