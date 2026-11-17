# Windows 与 SSH 架构

本文描述 Linux 和 Windows 的会话架构。

## 会话和客户端

本地终端和 SSH 终端运行同一个客户端编辑器。客户端从 Session 取得有效键位、命令目录和当前工作集，持有可编辑的文档副本、页面和选区。workspace daemon 保存已同步的未保存文档，执行文件、Git 和工作区插件操作。

```text
终端输入 → 有效键位 → 客户端编辑器 → 本地画面
                         ↕
               文档事务、资源订阅、命令与回执
                         ↕
                workspace daemon → 文件、Git、任务
```

按下前缀键、打开命令行和移动光标直接更新客户端状态。打开 Git 页面后，客户端显示缓存或加载状态，订阅远端仓库。收到资源更新时，只刷新对应订阅。命令面板、键位和插件通过相同的命令目录分派。

客户端维护分屏布局，每个分屏有自己的页面栈和选区。从命令行打开目录和在编辑器中打开 Files 使用同一条订阅路径。分屏复制数据页时，两个 View 共享插件 Buffer，分别接收各自的预览和选区更新。Session 用页面 ID 恢复对应 View，重连时可以按任意顺序重新订阅。

剪贴板属于客户端能力。`*` 寄存器请求 PRIMARY selection；具有相应 provider 的终端环境支持该选择，Windows 返回不支持。普通 `space-y` 使用 `+` 系统剪贴板。远端会话需要剪贴板操作时，由连接的客户端执行相应本地能力。

Unix 使用 Unix domain socket，Windows 使用 named pipe。Unix socket 放在私有运行目录并设置为仅所有者可访问；Windows 管道名包含用户 SID，访问控制授予当前用户。Windows 的 `--socket` 是逻辑端点名，日志位于 `%LOCALAPPDATA%\zess\run`（缺少该变量时使用临时目录）。实现见 [IPC](../crates/zess-ipc/src) 和 [Microsoft named pipes 文档](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipes)。

## 输入、确认与恢复

编辑首先应用到客户端文档。连接传送带文档身份和顺序号的事务，Session 返回已应用的版本。保存请求排在相应编辑之后，成功回执标明写入的版本。`:reload` 等待该文档先前的编辑得到确认。取得重载后的正文前，该文档显示等待状态，其他文档和数据页仍可操作。文件监听和工作区命令引起的文档变化经同一文档订阅返回。

执行数据页命令时，客户端随命令发送当时的条目选区和寄存器。Session 按这些条目处理命令，整文件 Git 操作在执行前验证所选路径。后续的选区移动只更新页面的显示位置。

快照标识 Session、server instance 和修订号。增量标明基线修订号，客户端发现缺口会请求新快照。文档与外部工作区命令各自跟踪回执。重连同一实例时，客户端重订阅资源、核实文档事务并查询已提交命令的结果。主动 detach 等待编辑和恢复位置同步完成；意外断线后，新客户端能恢复远端已确认的工作集。

Windows 远端启动 daemon 时检查当前 Job 的限制；需要且允许时显式请求 breakaway，遇到禁止 breakaway 的关闭即终止 Job 则报告错误。Microsoft 的 [OpenSSH Windows 命令进程实现](https://github.com/PowerShell/openssh-portable/blob/274893f8d1fe86c4d128b514c3af4bc10cdc0c99/contrib/win32/win32compat/w32-doexec.c#L440-L457) 使用关闭 Job 时结束进程并允许 breakaway 的设置。当前检查针对当前 Job；自定义嵌套外层 Job 的限制不在该检查保证范围内。

同一会话同时只允许一个活动交互客户端；另一个客户端附着会被拒绝。管理连接仍可查询会话。客户端或 SSH 断线不会结束 daemon；客户端自身退出后，其内存中尚未同步的编辑也会随之释放。daemon 崩溃或停止后，未保存内容、布局和撤销历史不能通过重连恢复。

## 连接和资源归属

会话管理器按规范化 workspace 路径选择端点。本地和 SSH 普通打开工作区时，首次连接持有临时 daemon。
客户端应用初始快照并确认 attach 后，daemon 才保留 Session 并允许发现与重连。
确认前取消或断开连接会回收临时进程和端点。显式 `session new` 创建可稍后 attach 的 Session。
daemon 负责 Session、文件及插件操作；终端客户端持有终端设备、剪贴板和连接。客户端退出后，服务端在连接释放后依据活动连接、未保存状态与会话策略决定是否停止，见 [配置](configuration.md)。会话运行在哪台机器，其 workspace、配置、Git、监听及外部程序就属于哪台机器；远程会话的界面和剪贴板仍属于本地客户端。

SSH 连接先探测远端平台并校验 Build ID 对应的服务端缓存；缓存不可用时，本机从发布产物下载匹配的服务端，经 SSH 上传、校验后安装，再以缓存路径启动 `remote-proxy`。Linux 发布包包含运行所需的动态链接器与库；安装时修补服务端的解释器路径，并分别校验发布包和安装后的文件。客户端与代理通过 Hello 确认 Build ID 一致；代理处理管理请求或连接对应的会话服务端并转发会话流。会话发现通过握手前的只读 Status 获取包含 Build ID 的状态，业务连接仍要求 Build ID 一致。一个连接的标准输入和输出承载协议，标准错误承载诊断；远端 shell 向标准输出写入 banner 会破坏协议。系统 SSH 按其有效配置负责认证、主机密钥检查和加密。Windows 命名管道忙时的连接尝试最多等待既有的两秒后才检查取消。

标准流代理复用 SSH 进程及现有本地 IPC，不需要在远端公开 TCP 监听或分配转发端口。代价是 SSH 进程和其标准流构成连接生命周期，须处理认证、握手、断线和流中诊断隔离；端口转发则需要另行管理转发端点及生命周期。这里没有对两种传输作性能比较。

## 平台能力的职责

根程序的运行时依赖装配一次选择各能力，管理器及会话使用同一组注入实例。`Platform` 提供主机、用户目录、绝对路径、进程 ID、时间和环境；`FileSystem`、`WatchBackend`、`ProcessRunner`、`LocalIpc` 和 `DaemonControl` 分别处理文件、监听、进程、本机连接与 daemon 生命周期。消费方只接收所需能力，而不是依赖一个包含全部操作的系统对象。监听句柄负责订阅生命周期，进程调用报告启动及退出错误；客户端剪贴板接口可以即时完成写入，也可以返回可轮询任务。Git 元数据查询、语法、grammar 工具链和脚本编译分别由 `GitQueries`、`SyntaxProvider`、`GrammarToolchain` 和 `ScriptCompiler` 管理。原生 Git、Helix 工具链和 Steel 在各自 provider 内使用其原生 I/O；注入的 `FileSystem` 负责 Zess 自身的路径、配置和资源准备，不拦截这些库的内部文件访问。语法资源按需加载，纯文本无需有效的 `ZESS_RUNTIME`。

## 相关设计参考

- [Neovim TUI](https://neovim.io/doc/user/tui/) 描述内置 UI 与编辑服务器分离；Zess 同样把界面与会话分开，但使用自身的快照、确认和恢复协议。
- [Emacs server](https://www.gnu.org/software/emacs/manual/html_node/emacs/Invoking-emacsclient.html) 允许客户端连接常驻进程；Zess 以 workspace 选择 daemon，并明确配置断开与停止。
- [Acme](https://9fans.github.io/plan9port/man/man4/acme.html) 通过文件接口暴露窗口和操作；Zess 使用类型化会话消息，保留客户端与编辑状态的边界。
- [VS Code Remote SSH](https://code.visualstudio.com/docs/remote/ssh) 说明远端服务器和扩展位置；[Zed Remote Development](https://zed.dev/docs/remote-development) 说明本地编辑器通过 SSH 连接远端。这里引用的是公开行为，不推断 VS Code SSH 扩展的内部实现。
- Zess 使用锁定的 [Helix core](https://github.com/helix-editor/helix/tree/ba40e547426b0f9896c8bdc699a4ab11f2b37dbc/helix-core) 的选区和编辑能力，客户端与 Session 共用文本事务和选区实现，不为终端界面引入整个 Helix Editor。
