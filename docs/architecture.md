# Windows 与 SSH 架构

本文描述本次跨平台和远程会话的设计。具体接口以源码为准，验证结果见 [实施报告](implementation-report.md)。

## 会话和客户端

每个 workspace 的 daemon 持有权威 `Session`。本地终端和 SSH 客户端通过同一会话协议提交输入，服务端执行编辑操作并发布状态。Ctrl-z 或连接中断后，重新连接同一 workspace 时可以继续使用仍在内存中的会话；正常退出则由服务端的会话策略决定是否停止。daemon 停止或崩溃后，内存中未保存的编辑内容不会由此机制恢复。

```text
终端输入 → 客户端 InputLedger → 连接 → workspace daemon → Session
终端显示 ← ClientUiState + RemoteStateCache ← snapshot / delta
```

`RemoteStateCache` 保存服务端发布的内容与呈现状态；客户端的 `ClientWorkspace` 持有 Buffer 成员和当前目标，依据动作回执更新，并决定关闭后的后继及是否退出。服务端保留已确认的工作集副本用于附着与重同步恢复。`ClientUiState` 保存通知等界面状态，`InputLedger` 跟踪输入和操作确认。会话保持依赖运行中的 daemon；设计不包括跨 daemon 崩溃的磁盘日志或多副本 CRDT。

剪贴板属于客户端能力。`*` 寄存器请求 PRIMARY selection；具有相应 provider 的终端环境支持该选择，Windows 返回不支持。普通 `space-y` 使用 `+` 系统剪贴板。远端会话需要剪贴板操作时，由连接的客户端执行相应本地能力。

Unix 使用 Unix domain socket，Windows 使用 named pipe。Unix socket 放在私有运行目录并设置为仅所有者可访问；Windows 管道名包含用户 SID，访问控制授予当前用户。Windows 的 `--socket` 是逻辑端点名，日志位于 `%LOCALAPPDATA%\zess\run`（缺少该变量时使用临时目录）。实现见 [IPC](../crates/zess-ipc/src) 和 [Microsoft named pipes 文档](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipes)。

客户端可以立即响应本地焦点和通知；编辑结果由 daemon 执行并发布，所以远程编辑的权威结果需要经过网络往返。典型断线恢复如下：

```text
客户端                     SSH 代理                  daemon / Session
  |-- 输入(seq) -------------->|------------------------>|
  |<-- Ack(seq) / 状态 --------|-------------------------|
  |       连接中断             |                         | 继续运行
  |-- 重新连接 / Attach ------>|------------------------>|
  |<-- instance、Ack、epoch、快照 -----------------------|
  |-- 安全时重发未确认输入；否则请求用户选择 ------------->|
```

## 输入、确认与恢复

一次编辑从客户端输入开始。客户端给待发送输入分配递增序号并保留在 `InputLedger`，daemon 执行后返回确认位置；客户端只移除已确认输入。显示使用服务端发布的语义快照及增量，本地界面状态负责通知、恢复选择等客户端交互，并不直接修改权威 Buffer。

快照标识 Session、server instance 和 revision；增量指定原 revision 和新 revision。身份不一致或 revision 不连续时，客户端暂停应用增量并请求新快照。Session 标识会话，server instance 区分不同 daemon；context epoch 用于识别输入的交互上下文是否发生变化。

恢复选择先于待决退出；失败或未知的操作结果保持可见。寄存器 `+p` 的待处理读取仍可被后续输入取消。连接中断后，客户端重新附着并等待服务端确认位置和快照。相同实例且服务端允许重放时，客户端重发尚未确认的输入。实例变化或上下文不允许重放时，用户选择重放或丢弃；丢弃已发送但未确认的输入时，等待服务端确认；尚未发送的队列直接丢弃。已确认输入不会因重连再发。带副作用的命令另以 request ID 和 server instance 跟踪，重连时查询结果；实例变化后无法假设旧实例的结果仍可查询。相关状态机见 [客户端](../crates/zess-terminal/src/lib.rs) 和 [服务端](../crates/zess-session/src/server.rs)。

Windows 远端启动 daemon 时检查当前 Job 的限制；需要且允许时显式请求 breakaway，遇到禁止 breakaway 的关闭即终止 Job 则报告错误。Microsoft 的 [OpenSSH Windows 命令进程实现](https://github.com/PowerShell/openssh-portable/blob/274893f8d1fe86c4d128b514c3af4bc10cdc0c99/contrib/win32/win32compat/w32-doexec.c#L440-L457) 使用关闭 Job 时结束进程并允许 breakaway 的设置。当前检查针对当前 Job；自定义嵌套外层 Job 的限制不在该检查保证范围内。

同一会话同时只允许一个活动交互客户端；另一个客户端附着会被拒绝。管理连接仍可查询会话。客户端或 SSH 断线不会结束 daemon；客户端自身退出后，其内存中的待确认输入也不再保留。daemon 崩溃或停止后，未保存内容、布局和撤销历史不能通过重连恢复。

## 连接和资源归属

本地管理器按规范化 workspace 路径选择端点，`ensure` 连接已有 daemon 或启动一个新 daemon。daemon 负责 Session、文件及插件操作；终端客户端持有终端设备、剪贴板和连接。客户端退出后，服务端在连接释放后依据活动连接、未保存状态与会话策略决定是否停止，见 [配置](configuration.md)。会话运行在哪台机器，其 workspace、配置、Git、监听及外部程序就属于哪台机器；远程会话的界面和剪贴板仍属于本地客户端。

SSH 连接依次探测远端系统和架构，选择预安装程序或 artifact，验证版本、协议、平台及适用的摘要；需要安装时通过 SSH 上传或由远端 HTTPS 下载，校验 SHA-256 后放入缓存。随后启动 `remote-proxy`，发送管理或附着请求；代理连接远端 workspace daemon，准备完成后握手并转发会话流。一个连接的标准输入和输出承载协议，标准错误承载诊断；远端 shell 向标准输出写入 banner 会破坏引导或协议。系统 SSH 按其有效配置负责认证、主机密钥检查和加密，Zess 不复制私钥，也不覆盖用户的主机密钥检查策略。Windows 命名管道忙时的连接尝试最多等待既有的两秒后才检查取消。

标准流代理复用 SSH 进程及现有本地 IPC，不需要在远端公开 TCP 监听或分配转发端口。代价是 SSH 进程和其标准流构成连接生命周期，须处理认证、握手、断线和流中诊断隔离；端口转发则需要另行管理转发端点及生命周期。这里没有对两种传输作性能比较。

## 平台能力的职责

根程序的运行时依赖装配一次选择各能力，管理器及会话使用同一组注入实例。`Platform` 提供主机、用户目录、绝对路径、进程 ID、时间和环境；`FileSystem`、`WatchBackend`、`ProcessRunner`、`LocalIpc` 和 `DaemonControl` 分别处理文件、监听、进程、本机连接与 daemon 生命周期。消费方只接收所需能力，而不是依赖一个包含全部操作的系统对象。监听句柄负责订阅生命周期，进程调用报告启动及退出错误；客户端剪贴板接口可以即时完成写入，也可以返回可轮询任务。Git 元数据查询、语法、grammar 工具链和脚本编译分别由 `GitQueries`、`SyntaxProvider`、`GrammarToolchain` 和 `ScriptCompiler` 管理。原生 Git、Helix 工具链和 Steel 在各自 provider 内使用其原生 I/O；注入的 `FileSystem` 负责 Zess 自身的路径、配置和资源准备，不拦截这些库的内部文件访问。语法资源按需加载，纯文本无需有效的 `ZESS_RUNTIME`。有关签名和注入入口见 [API 参考](api.md)。

## 相关设计参考

- [Neovim TUI](https://neovim.io/doc/user/tui/) 描述内置 UI 与编辑服务器分离；Zess 同样把界面与会话分开，但使用自身的快照、确认和恢复协议。
- [Emacs server](https://www.gnu.org/software/emacs/manual/html_node/emacs/Invoking-emacsclient.html) 允许客户端连接常驻进程；Zess 以 workspace 选择 daemon，并明确配置断开与停止。
- [Acme](https://9fans.github.io/plan9port/man/man4/acme.html) 通过文件接口暴露窗口和操作；Zess 使用类型化会话消息，保留客户端与编辑状态的边界。
- [VS Code Remote SSH](https://code.visualstudio.com/docs/remote/ssh) 说明远端服务器和扩展位置；[Zed Remote Development](https://zed.dev/docs/remote-development) 说明本地编辑器通过 SSH 连接远端。这里引用的是公开行为，不推断 VS Code SSH 扩展的内部实现。
- Zess 使用锁定的 [Helix core](https://github.com/helix-editor/helix/tree/ba40e547426b0f9896c8bdc699a4ab11f2b37dbc/helix-core) 的选区和编辑能力，Session 在服务端管理编辑状态，不为终端界面引入整个 Helix Editor。
