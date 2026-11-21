# LingXi DSH-TUI

一个用 Rust + Ratatui 写的 DeepSeek Harness 原生终端客户端。图形界面不是目标，**原生、轻、快**才是。没有 Electron，没有 Chromium，没有 WebView，没有 JS 运行时——只有终端和一个 Rust 二进制。

Agent 本身不做任何重写。Agent Loop、Provider 适配、工具调用、MCP、SubAgent、上下文管理、会话持久化全部来自官方 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)。官方运行时以**子模块 + 固定提交**的形式随本仓库一起构建、一起分发，用户不需要另外安装 DSH，客户端也不会在启动时联网拉取或升级运行时。

> 当前版本 `0.2.2-rc.5`，内置官方 DSH `0.2.2-rc.5`。产品版本号与内置 Agent 版本号**严格一致**。

## 定位

| | LingXi DSH-TUI | 官方 Desktop | 官方 CLI |
| --- | --- | --- | --- |
| 形态 | 原生 TUI | Electron | 终端 |
| 内存 | 单二进制，无独立渲染进程 | Chromium + Node | Node |
| 适用 | 长期挂着的会话、远程/SSH、低配机 | 图形交互 | 脚本化 |
| Agent | 官方运行时（随包分发） | 官方运行时 | 官方运行时 |

目标不是替代官方客户端，而是补上「终端里长期驻留、轻量、可远程」这一类场景。

## 架构

```text
┌──────────────────────┐        JSON-RPC 2.0 (stdio, newline-delimited)
│  dsh-tui (Rust)      │  <───────────────────────────────────────────────┐
│  ratatui + crossterm │                                                  │
└─────────┬────────────┘                                                  │
          │  frames                                                    │
          ▼                                                            │
┌──────────────────────┐     ┌──────────────────────────────────────────┴───┐
│  dsh-state           │     │  bundled DSH runtime (official, separate      │
│  event projection    │     │  process): Agent Loop / Provider / Tools /    │
│  folding / view      │     │  MCP / SubAgent / Sessions                    │
└─────────┬────────────┘     └───────────────────────────────────────────────┘
          ▼
┌──────────────────────┐
│  dsh-protocol        │  wire types, runtime lock, version comparison
└──────────────────────┘
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

前置：Rust stable（edition 2024，`rust-version = 1.85`）、Node.js ≥ 24.9.0、corepack（`pnpm@11.28.5`）。

```sh
git clone --recurse-submodules https://github.com/LingXiFox/LingXi-DSH-TUI.git
cd LingXi-DSH-TUI

# 只跑 UI：不需要构建运行时
cargo run -p dsh-tui

# 构建内置官方运行时（首次较慢，需要 Node + pnpm）
./scripts/build-runtime.sh
```

`dsh-tui` 支持 `--version` / `--help`；需要交互终端，否则以退出码 2 结束。日志写在 `.dsh-tui/dsh-tui.log`，`RUST_LOG` / `DSH_TUI_LOG` 控制级别。

### 提交前必跑

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
./scripts/check-runtime-pin.sh
./scripts/test-pin-guards.sh
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
3. **verify-rust** — 在该分支上跑 `fmt` / `clippy` / `test`。
4. **verify-runtime** — 用锁定的 Node / pnpm 版本真实构建一次运行时，并 smoke-test `--version`。
5. **open-pr** — 标题 `chore(runtime): sync DSH vX.Y.Z`，正文含当前/目标版本、完整 commit、release 链接、补丁与应用结果、协议契约结果、构建测试状态与人工复核清单。同一版本已有开放 PR 时不重复创建。
6. **auto-merge** — 默认**关闭**，需仓库变量 `AUTO_MERGE_UPSTREAM=true`。即便打开，也只在无冲突、所有检查通过、协议契约通过时合并；合并源码 ≠ 发布。
7. **report-failure** — 冲突、补丁失效、运行时构建失败、Rust 编译失败、协议契约被破坏、测试失败时，只维护**一个** issue，附错误摘要与 workflow 链接；旧版本继续生效。

PR 由 `GITHUB_TOKEN` 打开时不会触发 `pull_request` 类 workflow——这是 GitHub 的限制。仓库可选配置 `SYNC_APP_ID` / `SYNC_APP_PRIVATE_KEY` Secrets 改用 GitHub App token；未配置时 workflow 会显式告警，依赖本 workflow 内部的检查作为门禁。所有凭据只走 GitHub Secrets，不落日志。

### 发布

`release.yml` 仅手动触发（`workflow_dispatch`），默认 `dry_run=true` 只打包不发布。发布前会拒绝复用已存在的 tag。上游同步**永远不会**自动触发发布。

## 已实现 / 未实现

### 已实现（round 1：工程基建）

- [x] Rust workspace（Edition 2024，三个 crate，单向数据流）
- [x] 可运行的 Ratatui TUI：终端初始化 / 退出 / 恢复正确，降级模式可用（无 lock 文件也能起）
- [x] 官方运行时以子模块固定在 `dsh-v0.2.2-rc.5`（`9c2670d3bce5275e0d1610272958121af87787bb`）
- [x] 运行时锁定 + 校验工具链（`check` / `verify` / `contract` / `select` / `set`）
- [x] 官方 SDK 线路契约镜像（8 个 client 方法、6 个 server 通知、`approval/request`），带契约测试
- [x] 上游同步流水线（检测、升级分支、门禁、PR、失败 issue、受控 auto-merge）
- [x] CI（fmt / clippy / test / build / 运行时锁定 / shellcheck / 守卫反向测试）
- [x] 打包脚本骨架（`build-runtime.sh`、`package.sh`、`smoke-runtime.sh`）
- [x] 版本单源 + 版本漂移守卫（含反向用例）

### 未实现（明确列出，不假装有）

- [ ] **没有真正的 Agent 通信**。当前启动画面状态恒为 `detached`；不拉进程、不握手、不建会话。
- [ ] **没有对话 UI**。没有消息流、输入框、流式渲染、工具调用卡片、审批弹窗、取消。
- [ ] **没有 Provider 配置**、模型选择、密钥管理界面。
- [ ] **没有会话持久化界面的接入**（恢复 / 列举历史会话）。
- [ ] **没有 release 实际产物**。`release.yml` 是结构与流程，尚未产出过发布。
- [ ] **Windows 未验证**。CI 矩阵预留了 Windows，但未在任何 Windows 机器上实跑。
- [ ] **auto-merge 未在真实仓库验证**。逻辑已实现，需要打开仓库变量并触发一次真实同步才能证实。
- [ ] 性能基准（长会话 / 高频事件）未建立基线。

## 许可

本项目 MIT（见 `LICENSE`）。`runtime/upstream/` 保留上游 MIT 许可证与版权声明，未做任何删改。