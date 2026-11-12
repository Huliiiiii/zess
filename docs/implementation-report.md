# Windows 终端与 SSH 实施报告

本报告记录 Windows 终端、SSH 远程会话及其平台能力的实现和验证。用法与接口见 [配置](configuration.md)、[API 参考](api.md) 和 [架构](architecture.md)。

## 范围与状态

本轮交付 Windows 终端、本地会话、SSH 和其共享平台能力。TUI 粘贴、普通复制、寄存器、剪贴板 FIFO 和重连行为保留。已撤回本轮新增的原生 GUI 和 macOS 工作，既有 Unix 及 macOS 行为保留。

每个 workspace 的 daemon 持有权威 `Session`，终端通过本地连接或 SSH 标准流代理提交输入。客户端缓存快照并跟踪确认位置；断线重连会核对实例、上下文和输入确认，再决定重发或请求用户选择。daemon 保持运行时会话可以继续使用；daemon 停止或崩溃后，未保存的内存内容不由此机制恢复。同一会话同时只允许一个活动交互客户端。

系统 OpenSSH 负责认证、主机密钥检查和加密，策略遵循其有效配置。远端代理通过标准输入输出连接 workspace daemon；引导按平台选择预安装程序或 artifact，检查版本、协议及适用摘要，需要时上传或 HTTPS 下载。文件系统、监听、进程、本地 IPC、Git 查询、语法和脚本编译按职责注入。pager 使用程序和参数数组启动，Git 自身的 external diff 和 textconv 仍遵循 Git 语义。

## 平台调用链

审计覆盖根程序 `src` 与各 crate 的生产源码，排除 vendor、测试、benchmark、构建工具和纯路径计算，并追查 Helix、Steel、gix 的间接 I/O。

| 入口 | 保留的职责与边界 |
| ---- | ---------------- |
| 会话管理及本地 IPC | [SessionManager](../src/server.rs) 使用注入的文件系统、主机能力、`LocalIpc` 和 daemon 控制；Windows 使用 named pipe。 |
| 文件、监听和进程 | [`zess-platform`](../crates/zess-platform/src/lib.rs) 分别提供能力；进程使用程序和独立参数，取消及子进程生命周期由运行器处理。 |
| Git 与 pager | [GitQueries](../crates/zess-git/src/queries.rs) 负责仓库查询；[renderer](../crates/zess-git/src/renderer.rs) 通过 `Execution` 与进程运行器执行配置的 pager。 |
| 语法与脚本 | [SyntaxProvider](../crates/zess-text/src/language.rs)、[GrammarToolchain](../crates/zess-text/src/runtime.rs) 和 [ScriptCompiler](../crates/zess-script/src/compiler.rs) 分别负责语法资源、工具链和脚本编译。 |
| 会话和客户端身份 | [server](../crates/zess-session/src/server.rs) 与 [terminal](../crates/zess-terminal/src/lib.rs) 使用注入主机能力生成身份；客户端在重连时保留待确认输入及操作身份。 |

注入的文件系统负责 Zess 自身的路径、配置和资源准备；原生 Git、Helix 和 Steel 库内部 I/O 由各自 provider 负责。`ProcessRunner`、`ProcessJob` 及插件任务能力已实现。LSP 和通用任务宿主属于长期功能，并非本轮要求新建的功能。

## 已有验证及其范围

回退前的 Linux workspace 全套曾有 46 个测试程序、1026 项通过、7 项忽略；随后源码因撤回范围发生变化，因此不能将该结果视作当前源码的最终全套结果。此前 terminal 的恢复、会话分帧、SSH 引导和独立平台检查也有定点证据；回退后的检查列于下方。

Linux 私有 SSH 环境曾验证 TUI 的加密密钥认证、编辑保存、detach、重连和输入确认；会话管理覆盖 new、list、connect、stop、restart、dirty 内容拒绝和 force。该实验使用隔离后的 HOME、XDG、known_hosts 和 sshd 配置。早期一次启动在隔离前读取了用户 fish 配置并报告缺少工具；所检查配置及历史文件元数据均早于实验开始，未观察到这些文件被修改。后续通过登录 shell 启动阶段和 ForceCommand 入口的标记确认私有环境生效。

此前 Windows CLI 在 Wine 10 中使用官方 Win32 OpenSSH 10.4p1 Preview 连接私有 Linux sshd，完成远端程序上传、摘要校验、安装和会话管理；使用 Windows `ssh.exe` 的原始协议验证编辑、断线重连及确认去重。该测试不等于原生 Windows 实机，也不证明 Windows SSHD 或 Windows 服务端 PowerShell 引导。

此前 Linux 协议驱动经 Wine 中 Windows CLI `remote-proxy` 和 message named pipe 连接预启动 Session：编码总长 1,048,576 字节的合法输入及其后续帧均得到确认，保存结果逐字节匹配；1,048,577 字节的单帧被拒绝，重新连接后会话与已保存内容保持。Wine 实际读取 trace 和传输缓冲条件支持两帧进入同一次 Session 读取；Linux 是行为对照，没有同样的外部合批 trace。该实验使用的是回退前产物，未覆盖原生 Windows、Windows SSHD、Job 或端点发现。

Windows 强制停止此前验证了对其他程序的拒绝；Wine 对 `NtQueryInformationProcess` class 60 返回 `INVALID_INFO_CLASS`，合法 server 与错误角色的两项检查未能验证。Windows daemon 的 Job 测试中 silent breakaway 通过；另两项受 Wine 的 Job 查询实现影响而失败，不能记为通过。原生 Windows SSH、Job 和相关生命周期仍需实际环境验证。

Windows 服务端安装脚本的损坏缓存替换曾在 Linux PowerShell 7.7/.NET 中失败：`$null` 绑定为空字符串。脚本改用 [`[NullString]::Value`](https://learn.microsoft.com/en-us/dotnet/api/system.management.automation.language.nullstring?view=powershellsdk-7.4.0) 后，隔离环境测试覆盖首次安装、损坏上传保护、有效缓存不变、损坏缓存修复和临时文件清理。此结果使用 Linux 文件系统，不能代替 Windows PowerShell 5.1、NTFS 和 Windows SSHD 的实际验证。

此前完整测试通过不代表 strict clippy 全绿。`zess-session`、`zess-text`、`zess-script`、`zess-git`、`zess-services` 和 root 相关检查曾因既有 lint 失败；未为其添加抑制。回退后的检查结果列于下方。

## 最终检查

回退后 terminal 39 项、session clipboard 13 项、platform process 10 项及只读粘贴 1 项通过；Linux workspace all-targets 离线检查、格式和差异检查通过。移除 GUI 事件接口后重新运行 terminal 测试与 workspace 检查，均通过；Windows GNU 目标的 workspace all-targets 离线交叉检查和 CLI 构建通过。Wine 10 隔离环境中，CLI 的 `--version` 输出 `zess 0.1.0`，`remote-info` 返回 Windows x86_64、协议 2，报告的 SHA-256 与构建文件一致（`5b63b8f73a9b7c53f01d46b869ad9e63f8b25c809e3f7a4b9b669364cc989311`）。session 全库运行超过 60 秒后中断，未计为通过。

已重新安装 Linux TUI 到 `~/.cargo/bin/zess`，版本为 `zess 0.1.0`，SHA-256 为 `853a0050484aaa78fbe0970af591312ea2a54f74e02f21ea8b24fa73703ae160`。使用安装版 TUI 在隔离 Linux workspace 实际编辑并保存文件，断开后重新附着并确认显示保存内容；测试会话已停止。当前树尚未在原生 Windows SSH 环境验证。
