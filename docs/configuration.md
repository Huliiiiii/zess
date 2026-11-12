# 会话与远程配置

## 启动和管理会话

`zess` 在当前 workspace 启动或连接会话，并打开终端界面。指定 `--workspace DIR` 可选择本地 workspace。

以下是独立的管理命令示例。`SESSION_ID` 从 `session list` 的输出取得；连接后先按 Ctrl-z 断开，再执行需要管理该会话的命令。

```sh
zess session list
zess session new --workspace /path/to/project
zess session connect --session SESSION_ID
zess session stop --session SESSION_ID
zess session restart --session SESSION_ID
```

`restart` 和 `stop` 分别作用于仍在运行的会话；不要先 stop 再用同一 ID restart。restart 后会产生新的 Session ID，请重新 list 再选择会话。`session list` 显示会话 ID、进程、连接与修改状态及 workspace；`list`、`new` 和 `stop` 支持 `--json`。`new` 确保 workspace 的会话存在；文件参数用于新建的会话。复用已有会话时，要打开文件请通过编辑器或 connect 附着。stop 在有未保存内容时拒绝停止；`--force` 丢弃这些修改。restart 创建新的空会话，已保存文件保留，原会话的未保存内容、布局和撤销历史不会保留。远程会话也支持 `session restart --ssh HOST --session SESSION_ID`。Linux 上仅在 daemon 不响应管理协议、需要操作系统级强制停止兜底时，要求 Linux 6.5 或更新版本的 `SO_PEERPIDFD` 支持；正常的 stop 或 force 管理协议不依赖该兜底。

## 退出与会话保留

将以下内容写入 Wisp 配置文件。默认依次查找配置目录下的 `zess/init.w`、`zess/init.wisp`；也可用 `--config FILE` 指定文件。Windows 通常使用 `%APPDATA%\zess\init.w`，Linux 和 macOS 使用 `$XDG_CONFIG_HOME/zess/init.w`（未设置时为 `~/.config/zess/init.w`）。远程连接时配置文件位于远端。

客户端关闭最后一个未修改的无名草稿时退出，服务端默认在没有其他活动客户端和未保存内容时停止。通过 `set-session-options!` 设置客户端正常退出后的会话策略：

```wisp
set-session-options! : obj 'on-client-exit 'stop-if-unused
```

`on-client-exit` 接受 `keep` 或 `stop-if-unused`，默认 `stop-if-unused`。正常退出的客户端连接释放后，服务端确认没有其他活动客户端且会话没有未保存内容时停止。设置为 `keep` 时，正常退出也保留会话。意外断连和 Ctrl-z 始终保留会话。

`:exit` 显式请求停止会话；停止仍受未保存内容约束。

原 `on-quit` 和 `on-last-view` 编辑器选项已移除，请从配置中删除。

`ZESS_RUNTIME` 可指定语法资源目录。语法能力首次需要加载该目录时才检查它；路径无效会在该能力加载时报错，纯文本仍可正常使用。

## 剪贴板

普通 `space-y` 复制到系统剪贴板；`*` 寄存器请求 PRIMARY selection，是否可用取决于平台和本地剪贴板环境。Windows 不提供 PRIMARY。

Unix 终端普通复制通过 OSC 52 交给终端，需终端支持并允许该操作。读取使用本机 provider：Wayland 使用 `wl-paste`，X11 使用 `xclip` 或 `xsel`，macOS 普通剪贴板可使用 `pbpaste`。读取或写入失败时界面显示错误。远程会话使用客户端机器的剪贴板和 provider。

## Git diff pager

```wisp
set-view-options! 'git : obj 'diff-pager : obj 'program "delta" 'args : list "--paging=never"
```

`diff-pager` 是包含非空 `program` 字符串和可选 `args` 字符串数组的对象；省略 `args` 时使用空数组。对历史提交文件视图可同样配置 `git-commit-files`。程序直接以参数数组启动；旧的 shell 命令字符串不再接受。未设置时 Zess 不启动 pager，也不读取 `GIT_PAGER`、`pager.diff`、`core.pager` 或 `PAGER` 来选择 pager。

示例要求 `delta` 已安装在会话所在主机，且位于启动 daemon 时的 PATH。Zess 在会话所在主机的仓库目录启动该程序，将 diff 写入其标准输入，并解析标准输出中的 UTF-8 和 ANSI 样式后展示。示例中的 `--paging=never` 让 delta 直接输出，而不等待交互式翻页。源码跳转和 hunk 导航依赖可靠的行映射；渲染程序提供的映射不足时，不能假定展示行对应源码。程序启动失败、返回非零状态或输出非 UTF-8 时，渲染报告错误。

## SSH

以下编辑器启动与 connect 命令是互斥的附着方式；切换前先用 Ctrl-z 。

```sh
zess --ssh my-host --remote-workspace /path/to/project
zess session list --ssh my-host
zess session connect --ssh my-host --session SESSION_ID
```

`--ssh` 接受主机名或 SSH 配置别名，使用本机 `ssh` 及其有效配置、known_hosts 和认证方式；主机密钥的确认与拒绝取决于该配置中的 [`StrictHostKeyChecking`](https://man.openbsd.org/ssh_config#StrictHostKeyChecking) 等选项。可用 `--ssh-config`、`--ssh-identity`、`--ssh-user`、`--ssh-port` 和 `--ssh-jump` 覆盖对应连接参数。远端登录和 shell 启动脚本不得向标准输出打印 banner。远端 workspace 默认是 `.`，文件参数和 `--config` 指向远端路径。

引导首先检查远端已安装的兼容版本，然后检查缓存。必要时，`--server-binary FILE` 指定本地的目标平台可执行文件，或 `--server-artifacts FILE` 指定 artifact catalog；同平台没有显式 artifact 时可上传当前程序。不同平台需要兼容的预安装程序或指定对应 artifact。程序必须与远端操作系统、架构和运行时兼容。Zess 不提供默认下载地址。

catalog 是 JSON 数组，例如：

```json
[
  {
    "platform": { "os": "linux", "arch": "x86_64" },
    "version": "0.1.0",
    "sha256": "替换为可执行文件的 64 位十六进制 SHA-256",
    "source": { "kind": "local", "path": "zess-linux-x86_64" }
  }
]
```

将版本和摘要替换为实际值；版本需与本地 `zess --version` 一致。可用 `sha256sum zess-linux-x86_64` 计算 Linux 文件摘要，PowerShell 可用 `Get-FileHash .\zess.exe -Algorithm SHA256`。catalog 每个平台只能有一项；支持 `linux`、`windows` 和 `x86_64`、`aarch64`。本地相对路径相对于 catalog 所在目录；下载来源使用 `{"kind":"download","url":"https://example.com/zess-linux-x86_64"}`，需要远端 `curl`。URL 指向原始可执行文件，摘要也针对该文件；引导不解压归档。Windows 远端引导需要 PowerShell 和 `Get-FileHash`。下载及重定向只允许 HTTPS，安装前校验 SHA-256。

远端缓存位于 Unix 的 `~/.cache/zess/servers/<sha256>` 或 Windows 的 `%LOCALAPPDATA%\Zess\servers\<sha256>`。已安装程序在有选定 artifact 时也要匹配其摘要；无 artifact 时按版本、协议和平台匹配。摘要验证文件是否与 catalog 一致，catalog 和摘要本身应来自可信渠道。

本地和远端各自的用户目录、文件路径及外部程序属于各自主机。远端会话的编辑和文件操作发生在远端；剪贴板操作由本地客户端处理。SSH 的连接选项参见 [OpenSSH ssh 手册](https://man.openbsd.org/ssh) 和 [ssh_config 手册](https://man.openbsd.org/ssh_config)。

Windows PowerShell 中路径含空格时请引用，例如 `zess --workspace "C:\Users\Alice\My Project"`。终端界面需要可交互终端。默认 `Ctrl-z` 从终端断开，`:q` 关闭当前 View。
