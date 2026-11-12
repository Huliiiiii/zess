# 平台、远程连接与会话协议 API

本参考面向修改 Zess 的开发者。接口声明和消息字段以源码为准；各 crate 的公开类型可从其 `lib.rs` 查找。

## 平台能力

[`zess-platform`](../crates/zess-platform/src/lib.rs) 将系统能力拆成独立接口：

- `Platform` 提供 `host_info()`、`user_dirs()`、`current_dir()`、`current_exe()`、`absolute_path()`、`process_id()` 和 `system_time()`；`environment()` 接受 `OsStr` 并默认不暴露环境。`NativePlatform` 使用当前主机和系统目录策略。
- `FileSystem`、`WatchBackend` 和 `ProcessRunner` 分别提供文件系统、监听和进程能力。`NativeFileSystem`、`NativeWatchBackend` 和 `SystemProcessRunner` 是对应的原生实现。
- `ProcessRunner::environment()` 返回执行上下文的环境，自定义实现默认不暴露环境。`ProcessSpec` 表示程序、独立参数和控制台模式，`ProcessRunner` 启动或执行进程；`Cancellation` 为有界执行提供取消检查；`CancellationToken` 可克隆并共享取消状态。

[`Clipboard`](../crates/zess-terminal/src/clipboard.rs) 属于终端客户端，读取返回可轮询任务；写入可即时完成或返回可轮询任务。Windows 对 PRIMARY 读写返回 `Unsupported`；普通剪贴板使用本机 helper。该接口服务会话协议的剪贴板读取和写入。

## 会话管理

根程序内部的 [`SessionManager`](../src/server.rs) 通过 `native()` 构造原生实现，也可用 `new()` 注入平台依赖。`with_cancellation()` 为管理操作提供共享取消令牌。`entries()` 列出会话，`resolve()` 根据 ID 查找端点，`inspect()` 获取单个会话信息；`ensure()` 为 workspace 取得或启动会话，`stop()` 和 `restart()` 接受 force。调用者应处理未保存内容导致的停止失败。终端调用方可使用 `zess_terminal::run_connection_with_platform(factory, files, platform)` 注入主机能力。

## SSH 引导

[`zess-remote`](../crates/zess-remote/src/lib.rs) 导出以下接口：

- `SshTarget` 保存 host、可选端口、用户、identity 文件、SSH 配置文件、jump host及 `Option<CancellationToken>`。`command()` 生成 `ssh` 的 `ProcessSpec`，使用 `-T` 和 `ClearAllForwardings=yes`。
- `Bootstrap::prepare()` 准备远端程序并返回 `RemoteServer`。`SshBootstrap` 检测远端平台、检查预安装或缓存的程序，必要时安装选定 artifact；其 `file_system` 和 `platform` 分别注入 `Arc<dyn FileSystem>` 和 `Arc<dyn zess_platform::Platform>`。远程 crate 的 `Platform` 是操作系统与架构数据，不是主机能力 trait。
- `ArtifactCatalog` 按平台索引 `ServerArtifact`。artifact 包含平台、版本、SHA-256 和本地文件或 HTTPS 下载来源。catalog 文件中相对的本地路径相对于 catalog 文件解析。
- `SshConnector` 将引导、进程启动和初始请求组合为 `SshConnection`。`Connector::connect()` 返回连接。

引导匹配远端程序的版本、代理协议版本和平台；选定 artifact 时还匹配二进制摘要。无兼容远端程序且无法取得匹配 artifact 时，引导返回错误。远端启动脚本必须保持标准输出无 banner，以免破坏引导输出和协议流。

## 代理与会话消息

代理初始请求是换行分隔的 JSON。`ProxyRequest::new()` 设置当前 `PROXY_PROTOCOL_VERSION`；`validate()` 拒绝不同版本。`ProxyOperation` 包含 `AttachSession`、`Attach`、`List`、`Start`、`Restart`、`Stop` 和 `Status`，字段和序列化形式见 [定义](../crates/zess-remote/src/lib.rs)。代理完成请求准备后返回 `ProxyHandshake::Ready { protocol_version }`；准备失败时返回 `Failed { message }`。当前代理协议版本为 3，客户端检查握手版本。随后管理请求返回管理响应，附着请求进入会话协议流。标准输出不得混入诊断文字。

[`zess-protocol`](../crates/zess-protocol/src/lib.rs) 定义 `ClientMessage`、`ServerMessage`、快照、增量及操作结果。快照和增量携带 session、server instance 和 revision；客户端在应用增量前检查身份与基础 revision。命令通过 `RequestId` 和 server instance 标识，结果为 `Pending`、`Completed { outcome, state_revision }` 或 `Unknown`。完成的 outcome 可表示已应用、已保存、Buffer 已关闭及实际后继，或失败；取消可体现为失败信息，剪贴板写入结果另有 `Cancelled`。`Unknown` 表示结果无法确认；普通命令显示提示，仍在服务端待处理的客户端动作则解除提交标记，允许重新提交并由动作 ID 防止重复消费。输入序列和确认用于跟踪客户端输入进度；重连是否允许重放由服务端返回的 `replay_allowed` 决定。

消息使用 [`zess-ipc::framing`](../crates/zess-ipc/src/framing.rs) 的换行分隔 JSON。读取方必须执行消息大小限制；会话协议定义的上限为 `MAX_MESSAGE_BYTES` 和 `MAX_INPUT_BYTES`，代理初始请求另有独立上限。协议类型可能随客户端和服务端一起演进，集成时应使用匹配版本。

`SessionSnapshot` 包括可见 View、surface、布局、焦点、提示、渲染计划、Buffer 事实清单、已确认工作集和待处理客户端动作；`SessionDelta` 表达对应的变化。附着和完整重同步恢复客户端工作集，正常更新通过 `Open`、`Activate`、`Forget` 及关闭回执修改本地成员。`AcknowledgeClientAction` 确认成员动作，`CloseBuffer` 携带客户端选择的目标和后继，`ClientExit` 只结束对应客户端连接。客户端渲染使用语义数据及其标识，不以屏幕字符作为文件或选区的权威来源。`Attach` 携带客户端身份、尺寸、文件和先前 context epoch；`Attached` 返回实例、确认序号、当前 epoch 和是否允许重放。`Resync` 请求完整快照。`Command`、`QueryOperation` 和 `ResultAck` 分别用于提交、查询和确认命令结果。确切字段及变体见 [协议定义](../crates/zess-protocol/src/lib.rs)。

当前 `MAX_MESSAGE_BYTES` 为 16 MiB，`MAX_INPUT_BYTES` 为 1 MiB。服务端对客户端输入按单条编码消息计算上限，包含末尾换行；同次读取中的多条完整消息各自计数。

本地 IPC 的 [`LocalIpc`](../crates/zess-ipc/src/transport.rs) 负责发现、定位、准备、检查、连接和绑定端点，并提供日志路径；[`DaemonControl`](../crates/zess-ipc/src/platform.rs) 管理 daemon 进程。`SocketPath` 是平台相关的逻辑端点：Windows 对应用户私有的 named pipe，Unix 对应文件系统中的 socket。调用方不应自行拼接跨平台端点。`LocalIpc::prepare` 准备端点所需的平台资源；`is_present` 和 `log_path` 的默认实现使用原生端点；自定义端点空间应覆盖它们。根程序装配 `NativeLocalIpc`，准备端点后调用 `zess_session::serve_with_platform(&ipc, &socket, session, host.as_ref())`，服务端通过注入的 IPC 绑定监听。`serve` 是使用 `NativePlatform` 的便利入口。

## 装配示例

下例将三个原生后端注入服务，再把服务交给 Session；`with_services` 选择原生语法和编译器。`workspace` 是现有目录；创建过程会规范化并检查目录，错误由调用方处理。

```rust
use std::{path::PathBuf, sync::Arc};
use zess_platform::{NativeFileSystem, NativeWatchBackend, SystemProcessRunner};
use zess_services::NativeServices;
use zess_session::Session;

fn make_session(workspace: PathBuf) -> anyhow::Result<Session> {
    let services = NativeServices::with_backends(
        workspace.clone(),
        Arc::new(NativeFileSystem),
        Arc::new(SystemProcessRunner),
        Arc::new(NativeWatchBackend),
    )?;
    Session::with_services(workspace, &[], None, services)
}
```

默认装配可用 `NativeServices::new(root)` 和 `Session::new(workspace, files, config)`。`Platform::user_dirs()` 返回 `UserDirs`，路径是 `PathBuf`，环境决定的运行目录为 `Option<PathBuf>`；文件系统操作使用标准 `Path`、`PathBuf` 和 `io::Result`，无需先转换成 UTF-8 字符串。`open_read` 返回 `Box<dyn Read + Send>`，`metadata` 返回 `std::fs::Metadata`；目录项以 `DirectoryEntry` 的路径、`OsString` 文件名及平台无关 `FileType` 字段传递。`NativeServices::with_backends` 接受的三个后端均为 `Arc<dyn ...>`，可分别替换；Session 从传入服务取得文件系统，调用方应使服务与 workspace 指向同一工作区。

进程调用先用 `ProcessSpec::new(program)`，再用 `arg`、`args`、`current_dir`、`env` 设置独立参数和环境。`ProcessRunner::output(&spec, input, cancellation)` 收集结果；`spawn(&spec)` 返回 `OwnedProcess`，其句柄持有进程树，释放时终止剩余子进程并回收主进程。需要后台轮询时，`ProcessJob::start(runner, spec, input, timeout, output_limit)` 启动任务，`poll()` 返回 `None` 表示尚未完成；释放任务请求取消，工作线程负责清理。父进程退出后也会清理剩余后代。

在 Unix 有控制终端且调用者位于前台时，`ConsoleMode::Inherit` 将终端前台交给子进程。长期运行的 SSH 进程在代理 Ready 后调用 `OwnedProcess::release_console()`，归还前台控制权以恢复编辑界面；失败和释放句柄时也会尝试归还。具体的输出模式、生命周期和错误见 [进程接口](../crates/zess-platform/src/process.rs)。

`ConnectionFactory::connect()` 每次建立新的字节流，供首次连接及重连使用；`connect_with_cancellation(&CancellationToken)` 的默认实现只在调用 `connect()` 前检查一次。可能阻塞的实现应覆盖该方法，在等待期间检查取消。`SessionManager::new(filesystem, ipc, daemons, host)` 在根程序内部装配这四项能力；`native()` 使用原生实现，`with_cancellation(token)` 绑定取消。

## 业务能力注入

根程序的运行时依赖装配选择 `FileSystem`、`ProcessRunner`、`WatchBackend`、`GitQueries`、`SyntaxProvider`、`GrammarToolchain`、`ScriptCompiler` 和主机能力。管理器的端点、连接、存在性及日志操作使用其注入的 IPC；server 通过 `NativeServices::with_git_queries` 和 `Session::with_capabilities` 消费同一组能力。`NativeServices::new`、`with_backends` 和 `Session::new`、`with_services` 仍提供原生默认装配。

[`GitQueries`](../crates/zess-git/src/queries.rs) 包含仓库发现、变更统计和对象查询；`NativeGitQueries` 使用 libgit2/gix 的原生仓库 I/O；Git 外部命令经 `Execution` 委派，services 的实现将环境查询转给 `ProcessRunner`。[`SyntaxProvider`](../crates/zess-text/src/language.rs) 拥有语法资源及可能发生的延迟加载，语言和语法值应交还创建它们的 provider；[`GrammarToolchain`](../crates/zess-text/src/runtime.rs) 封装 grammar 获取、构建与进度回调。[`ScriptCompiler`](../crates/zess-script/src/compiler.rs) 创建引擎并处理各编译入口；`NativeSteelCompiler` 使用 Steel 的原生相对导入、文件读取和编译缓存。`FileSystem` 不接管这些原生库的内部 I/O。

`NativePlatform::absolute_path` 使用标准库路径语义，包括 Windows 的盘符相对和根相对路径。客户端 ID 使用主机提供的 PID、时间及进程内 nonce；同一客户端的跟踪输入和命令共用请求序号分配，重连沿用未完成操作的 ID。server instance 也由注入的主机身份与时间生成。
