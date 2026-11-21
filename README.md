# LingXi DSH-TUI

一个用 Rust + Ratatui 写的 DeepSeek Harness 原生终端客户端。Rust 进程负责终端界面，官方 Agent 在独立 Node.js 进程中运行。

Agent 本身不做任何重写。Agent Loop、Provider 适配、工具调用、MCP、SubAgent、上下文管理、会话持久化全部来自官方 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)。官方运行时以**子模块 + 固定提交**的形式随本仓库一起构建、一起分发，用户不需要另外安装 DSH，客户端也不会在启动时联网拉取或升级运行时。

> 当前版本 `0.2.2-rc.5`，内置官方 DSH `0.2.2-rc.5`。产品版本号与内置 Agent 版本号**严格一致**。

## 定位

| | LingXi DSH-TUI | 官方 Desktop | 官方 CLI |
| --- | --- | --- | --- |
| 形态 | 原生 TUI | Electron | 终端 |
| 进程 | Rust TUI + Node Agent | Chromium + Node | Node |
| 适用 | 长期挂着的会话、远程/SSH、低配机 | 图形交互 | 脚本化 |
| Agent | 官方运行时（随包分发） | 官方运行时 | 官方运行时 |

适用于终端里长期驻留的会话与远程访问。

## 架构

```text
Official DSH runtime (separate Node.js process)
          │ JSON-RPC 2.0 on stdio, newline-delimited
          ▼
dsh-protocol  →  dsh-state  →  dsh-tui
wire types      projection    Ratatui rendering and keyboard input
runtime lock    folding       terminal lifecycle
```

三个 crate，职责单向流动，互不越界：

- **`crates/dsh-protocol`** — 与运行时通信的一切：SDK 帧类型、运行时锁定文件解析、版本比较与发布筛选，以及 `dsh-runtime-lock` 工具。不画界面、不存 UI 状态。
- **`crates/dsh-state`** — UI 状态与投影：事件投影、工具调用聚合、消息折叠、会话视图模型。不启动进程、不碰终端。
- **`crates/dsh-tui`** — Ratatui 布局、渲染、按键映射、终端生命周期。不手写 JSON-RPC 解析。

进程边界是硬约束：Agent 永远是**独立进程**，通过官方 SDK 的 stdio JSON-RPC 通信；从不链接进 Rust 进程，也不复刻任何协议细节。

## 与上游的关系

- 官方仓库：[`deepseek-ai/deepseek-harness`](https://github.com/deepseek-ai/deepseek-harness)（MIT）
- 子模块：`runtime/upstream`，固定在**发布 tag** 对应的提交上，**从不跟随默认分支**
- 锁定记录：`runtime/upstream.lock.toml`（仓库 / tag / 版本 / 完整 commit / 子模块路径 / 构建配方）

上游源码保持原样。确实必须的改动形式化地放在 `patches/`，由打包脚本显式应用，同步流水线会验证补丁是否仍能干净应用。

## 本地开发

构建需要 Rust stable（edition 2024，`rust-version = 1.85`）。构建官方运行时另需 Node.js ≥ 24.9.0 和可用的 `pnpm@11.28.5`，构建脚本使用已配置的包管理器。

```sh
git clone --recurse-submodules https://github.com/LingXiFox/LingXi-DSH-TUI.git
cd LingXi-DSH-TUI

# 构建内置官方运行时（首次较慢，需要 Node + pnpm）
./scripts/build-runtime.sh

# 验证官方 SDK 握手与退出
cargo run -p dsh-tui -- --check-runtime

# 启动 TUI 并连接已构建的官方运行时
cargo run -p dsh-tui
```

`dsh-tui` 支持 `--version`、`--help`、`--check-runtime`、`--history` 和 `--replay-journal FILE`。界面需要交互终端，否则以退出码 2 结束。`--check-runtime` 可在 CI 中验证初始化与正常退出，失败返回非零退出码。

客户端启动官方 `sdk` profile，使用 `workspace-write` 权限模式。默认路由是 `deepseek-official` / `deepseek-v4-pro`，可用 `DSH_TUI_PROVIDER` 和 `DSH_TUI_MODEL` 设置。运行时通过官方配置读取凭据。工具审批默认选中拒绝，选择「允许一次」后才会放行。

`DSH_TUI_ROOT` 可指定工作区或包目录。包目录也可由可执行文件位置自动识别。Agent 使用该目录下 `.dsh-tui/agent` 作为独立 `DSH_HOME`，遥测关闭。TUI 日志写入 `.dsh-tui/dsh-tui.log`，`RUST_LOG` / `DSH_TUI_LOG` 控制级别。官方运行时的 stderr 写入 `.dsh-tui/runtime.log`。

### 对话和工具详情

| 按键 | 操作 |
| --- | --- |
| Enter | 发送当前草稿 |
| Ctrl+J / Shift+Enter | 换行。若终端无法区分 Shift+Enter，可使用 Ctrl+J 或设置 `DSH_TUI_NEWLINE=alt-enter` 后使用 Alt+Enter |
| Shift+方向键 / Ctrl+A | 选择文本 |
| Ctrl+P | 打开命令面板，输入命令后按 Enter |
| Ctrl+L | 切换 Compact、Normal、Verbose |
| Tab | 展开或折叠工具组、单个调用或子会话摘要 |
| F6 / Esc | 切换输入与历史焦点，或关闭浮层、从子会话返回 |
| PgUp / PgDn | 翻页。历史焦点下 End 恢复跟随最新消息 |
| Ctrl+C | 中断当前任务并重连，空闲时退出 |
| ? | 查看帮助 |

Compact 按轮次显示工具数量与状态。Normal 按执行步骤展示调用及输出摘要。Verbose 提供参数、输出和错误的完整文本。

在命令面板中，`/new` 开始新会话，`/history` 打开可搜索的会话选择器，`/resume ID` 恢复已知会话，`/cwd` 查询会话目录。空闲时可用 `/route PROVIDER MODEL` 重连到指定模型路由。`/failure` 跳到失败工具，`/view ID` 查看已经收到事件的子会话，`/draft` 取回发送失败的草稿，`/quit` 退出。

### 使用已有 DSH 配置

```sh
cargo build --release --locked -p dsh-tui
DSH_TUI_DSH_HOME="$HOME/.dsh" ./target/release/dsh-tui --history
```

启动后默认是 Compact。历史列表按日期、ID 或工作目录过滤，方向键选择，Enter 通过官方 SDK 恢复会话的独立副本。此后的写入留在 `.dsh-tui/ecosystem-*`。

| 命令 | 操作 |
| --- | --- |
| `/models` | 浏览官方 Provider 和模型。Enter 切换路由 |
| `/plugins` | 查看官方 SDK 加载的插件及状态 |
| `/skills` | 浏览官方发现的 Skills。选择可调用项后把 `/技能名` 放入草稿 |
| `/mcp` | 查看官方 MCP 服务和连接状态 |
| `/reload` | 重载已支持的配置，重启 SDK 并恢复当前会话 |

配置兼容范围覆盖官方 Provider 配置、Skills 和 MCP 配置。MCP 默认禁用连接。审阅服务配置后可在启动时设置 `DSH_TUI_ENABLE_MCP=1`。配置来源、共享历史策略、验证结果和限制见 [P5 开发记录](docs/p5.md)。

### 本地历史与离线回放

TUI 将收到的 SDK 事件和 Host 请求写入 `.dsh-tui/history/`，重启后可回填已记录的正文、工具结果和子会话。这个目录可能包含私密会话内容，不应上传或提交。工具输出与正文按需从磁盘加载，Markdown 布局使用有上限的缓存。

已有官方 DSH 会话可用锁定版本的读取器导出，再用 `dsh-tui --replay-journal FILE` 只读打开。离线模式不启动 Agent、不调用 Provider、不执行工具，也不会把导入会话加到在线历史。命令和支持格式见 [P4 开发记录](docs/p4.md#回放既有-dsh-会话)。

### 提交前必跑

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
./scripts/check-runtime-pin.sh
./scripts/test-pin-guards.sh
./scripts/test-sync-merge.sh
```

## 版本管理

**单一事实来源**：`runtime/upstream.lock.toml`。`Cargo.toml` 里的 `[workspace.package].version` 是它的镜像，由工具同步，不是手改对象。

- 产品版本 == 内置 DSH 版本。看到 `0.2.2-rc.5`，就意味着包里是 DSH `0.2.2-rc.5`。
- 纯 UI 修复**不动版本号**，用构建号（`DSH_TUI_BUILD_ID`，通常是 git commit）区分。
- 升版本必然同时移动运行时提交：`dsh-runtime-lock set --tag <tag> --commit <sha>` 一步完成，手改无效。
- 已发布的 tag 不可变，不覆盖、不重写、不强推。
- 上游发了新版本不等于用户会被升级——升级只发生在**本仓库发布 release** 时。

守卫脚本：

```sh
./scripts/check-runtime-pin.sh                                   # 纯 bash，CI 与本地都跑
cargo run -p dsh-protocol --bin dsh-runtime-lock -- check        # lock + 子模块 + 协议契约
./scripts/test-pin-guards.sh                                     # 守卫本身的反向用例
```

## 自动化：上游跟踪

`.github/workflows/upstream-sync.yml`：每 6 小时（cron `17 */6 * * *`）与手动触发。只读**已发布 Release**，不跟默认分支。

流程：

1. **detect** — 拉取全部 releases，交给 `dsh-runtime-lock select` 判断是否有更新的版本（含稳定版 / RC 排序，杜绝降级）。无更新则直接结束，不建 PR。
2. **prepare** — 建专用升级分支，把子模块移到该 tag 的提交，同步 lock 文件与工作区版本，校验补丁、协议契约，生成协议差异摘要。
3. **verify-rust** — 对准备阶段产出的完整 commit 跑 `fmt` / `clippy` / `test` / `build` 与守卫回归测试。
4. **verify-runtime** — 用锁定的 Node / pnpm 版本真实构建一次运行时，并 smoke-test `--version`。
5. **open-pr** — 标题 `chore(runtime): sync DSH vX.Y.Z`，正文含当前/目标版本、完整 commit、上游发布说明、协议差异与验证结果。同一版本重用已有 PR，重跑也保留升级分支的历史及维护者修复。手动 `dry_run=true` 会准备并推送验证分支。
6. **auto-merge** — 默认关闭，由仓库变量 `AUTO_MERGE_UPSTREAM=true` 开启。合并前等待全部检查，核对三平台 CI 与运行时检查齐全通过、当前 head 等于已验证 commit、分支可合并，以及人工阻止标签和 review 状态。`runtime-sync/blocked` 由机器人维护，人工可用 `no-auto-merge`、`do-not-merge`、`breaking-change`、`major-risk`、`hold` 或 `blocked` 阻止合并。
7. **report-failure** — 每个目标版本维护一个 issue，重跑更新正文，附失败任务、错误日志片段及 workflow 链接。

PR 打开或刷新后，流水线显式触发 `CI` 与 `Runtime check`。开启自动合并时，流水线等待本次触发的运行完成，并核对两次运行验证的提交。创建 PR 需要配置具有 Pull requests 写权限的 GitHub App（`SYNC_APP_ID` / `SYNC_APP_PRIVATE_KEY`），或在仓库设置允许 GitHub Actions 使用 `GITHUB_TOKEN` 创建 PR。

`runtime/protocol-source.lock` 记录已审阅 SDK 协议源码的 Git blob 哈希。文件、字段、方法或传输实现变化都会使 `contract` 失败。维护者审阅 Rust 类型及测试后更新快照。`Runtime check` 构建官方运行时并通过 PTY 验证两轮对话、真实文件读取、会话恢复和进程回收。

### 发布

`release.yml` 仅手动触发（`workflow_dispatch`），默认 `dry_run=true` 只打包不发布。发布前会拒绝复用已存在的 tag。上游同步**永远不会**自动触发发布。

## 当前限制

固定版本的 SDK 在消息落定后发送 `session.event`，其中可能包含打包的模型增量。目前没有逐 Token 推送。配置 `DSH_TUI_DSH_HOME` 后，`/history` 通过官方持久化组件浏览历史，并通过 SDK 恢复当前格式会话的独立副本。其他启动方式下，`/history` 显示 TUI 自己记录的会话。模型路由由官方 SDK 校验。当前 SDK 以进程为单位处理中断，TUI 通过 `shutdown`、限时回收和重新连接中断任务，会影响当前进程管理的所有任务。

开发流程、基准与验收结果见 [P2/P3 开发记录](docs/p2-p3.md)、[P4 开发记录](docs/p4.md)和 [P5 开发记录](docs/p5.md)。

## 许可

本项目 MIT（见 `LICENSE`）。`runtime/upstream/` 保留上游 MIT 许可证与版权声明，未做任何删改。
