# roster

管理 **agent 会话**的命令行工具：一个全屏 TUI 用来浏览、筛选、重命名、删除会话和改工作区，外加一组可以脚本化的 CLI 子命令。Rust 编写，单二进制。

目前支持两种会话存储：

| 来源 | 存储 | 说明 |
| --- | --- | --- |
| **DST / dsh-tui** | JSONL 日志，每个会话一个目录 | `~/.dsh/sessions/**/session.vN.jsonl[.zstd]` |
| **opencode** | 一个共享的 SQLite 数据库 | `~/.local/share/opencode/opencode.db` |

两种都是直接读写磁盘，不经过 dsh-tui / opencode 进程，所以关掉它们之后也能整理历史。TUI 里按 `s` 在「全部 / DST / opencode」之间切换。

## DST 的数据长什么样

```
<dsh_home>/sessions/--E-home-alice-code-Siren--/<session-id>/
                                 └─ session.v4.jsonl.zstd
        └─ 由 cwd 编码出来的工作区目录名
```

- **目录名 = 路径编码**。`/ \ :` 折叠成单个 `-`，其余不安全字符变成 `~XXXX`（UTF-16 码元的大写十六进制），整体包上 `--…--`，最长 251。`--E-code-Rhine~0020Music--` 就是 `E:\code\Rhine Music`。
- **日志 = JSONL；压缩后是「多帧 zstd 拼接容器」**。第 0 帧必须恰好是一行 header：

  ```json
  {"type":"session","version":4,"id":"…","createdAt":1710000000000,"cwd":"E:\\code\\x","parentSession":null,"agentPreset":"…"}
  ```

  之后的每一帧是一批事件（`user/message`、`session/title`…），每行带 `seq`。
- **不变量：读端要求第一帧只有一行**。改 header 时不能顺手把后续内容追加进去，必须换成一个新的独立帧。

工具靠 `src/zstd_frames.rs` 里的扫描器在不全量解压的前提下定位帧边界——追加标题、只重写第一帧、统计帧数都是这么做的。

## opencode 的数据长什么样

opencode（1.x）把一切放进一个库：`opencode.db`。对本工具重要的表：

| 表 | 用途 |
| --- | --- |
| `session` | 会话的**物化视图**：id、slug、title、directory、project_id、parent_id、agent、model、token 统计、时间戳 |
| `project` | 项目，`worktree` 是 git 工作区路径，id 是仓库**根提交**的 hash |
| `message` / `part` | 消息与消息片段，`session_id` / `message_id` 关联 |
| `event` + `event_sequence` | **事件日志才是真相**：每次变更按 `aggregate_id`（= 会话 id）追加一条 `session.*` / `message.*` 事件，`seq` 逐条递增 |

因此本工具的写操作不是简单 UPDATE 一下就完事：

- **重命名**：先读出该会话最新的一条 `session.*` 事件，改它的 `info.title`，以**同样的类型**追加到 `event`（`seq` 取聚合计数器的下一个值，同时更新 `event_sequence`），再更新 `session.title`——一个事务里完成。这样 opencode 重放事件时看到的也是新标题。
- **改工作区**：更新 `session.directory`，并把 `path` 重算成相对工作区的形式；如果新目录落在某个已知项目的工作区里，就顺带把 `project_id` 指过去（取最长匹配的前缀）。**不会凭空创建 project**——opencode 的项目 id 是仓库根提交 hash，编不出来。
- **删除**：逐表删掉会话、消息、片段、待办、事件（不依赖 `ON DELETE CASCADE`，以免历史 schema 不一致），最后清掉 `storage/session_diff/<id>.json`。**不保留副本，删了就没了。**

列名在 opencode 各版本间有过变动（`cost`、token 计数、`path` 都是后加的），所以读取全部走列名查表，缺的列直接跳过。

## 构建

需要 stable Rust（仓库里的 `rust-toolchain.toml` 会拉取 `rustfmt` + `clippy`）。SQLite 是 bundled 编译的，Windows 上需要 MSVC 工具链（VS Build Tools 即可）。

```console
$ cargo build --release
$ ./target/release/roster --help
```

### 图标与 Windows 资源

`build.rs` 用 [`winres`](https://crates.io/crates/winres) 把 `package/icon.ico` 和版本信息嵌进 exe，所以在资源管理器里右键看属性、任务栏、Alt-Tab 都是新图标：

| 资源 | 来源 |
| --- | --- |
| 图标 | `package/icon.ico` |
| ProductName / InternalName / OriginalFilename | `CARGO_PKG_NAME`（即 `roster`） |
| FileVersion / ProductVersion | `CARGO_PKG_VERSION` |
| FileDescription | `CARGO_PKG_DESCRIPTION` |

因为名字取自 `Cargo.toml`，以后再改名不会在属性页留下旧名字。非 Windows 目标会跳过整段资源编译。

`package/icon.svg` 是图标源文件，`package/background.svg` 是它的底板；`icon.ico` 需要从 svg 重新导出（多个尺寸），例如：

```console
$ magick -background none package/icon.svg -define icon:auto-resize=256,128,64,48,32,16 package/icon.ico
```

只改 svg 不会影响 exe——**要重新导出 ico 才会生效**。

## 用 TUI

不带参数运行即进入 TUI：左边是会话列表（`DST` / `OC` 是来源角标），右边是详情，底部一行状态栏。

| 按键 | 作用 |
| --- | --- |
| `j` / `↓`、`k` / `↑` | 上下移动 |
| `PgUp` / `PgDn` | 一次移动 10 行 |
| `g` / `G` | 跳到首/尾 |
| `/` | 按标题、路径或 id 筛选（`Ctrl+W` 删词，`Ctrl+U` 清空，`Esc` 还原） |
| `s` | 切换来源：全部 → DST → opencode |
| `r` | 重命名 |
| `w` | 改工作区（DST 改 header cwd 并移动目录；opencode 改 `directory` 并重算项目归属） |
| `d` | 删除（永久删除，不可撤销） |
| `R` | 从磁盘重新加载 |
| `L` | 切换语言：跟随系统 → 简体中文 → English |
| `?` | 帮助 |
| `q` / `Esc` / `Ctrl+C` | 退出 |

详情面板对两种来源显示不同的字段：DST 显示日志文件、编码、帧数、字节数；opencode 显示数据库路径、消息数、事件数、token 与花费。被运行中的 dsh-tui 挂载的 DST 会话会带 pid 警告——它不阻止操作。

## 用 CLI

```console
$ roster --list                     # id、cwd、标题、来源，制表符分隔
$ roster --list --json              # 结构化的完整字段
$ roster --list --source opencode   # 只看一个来源
$ roster --rename <id> <标题>
$ roster --delete <id>              # 永久删除，不回收
$ roster --set-cwd <id> /abs/path
```

会话 id 可以只写**无歧义的前缀**；DST 的 id 是 uuid，opencode 的是 `ses_…`，两者不会混淆。

| 选项 | 说明 |
| --- | --- |
| `--source <all\|dst\|opencode>` | 只显示某个来源，默认 `all` |
| `--dsh-home <路径>` | DSH 主目录，默认 `$DSH_HOME`，再退到 `~/.dsh` |
| `--data-dir <路径>` | dsh-tui 数据目录，默认 `~/.dsh-tui` |
| `--sessions-root <路径>` | 额外的 DST 会话根目录，可重复 |
| `--opencode-dir <路径>` | 额外的 opencode 数据目录，可重复 |
| `--lang <auto\|zh-CN\|en>` | 界面语言，`auto` 表示跟随系统并清掉已保存的选择 |

### 目录发现

**DST 会话根**按顺序收集并去重（大小写不敏感）：`--sessions-root` → `$DSH_TUI_SESSION_ROOT` → `<dsh_home>/sessions` → `<data_dir>/sessions`。

**opencode 数据目录**：`--opencode-dir` → `$OPENCODE_DATA_DIR` → `$XDG_DATA_HOME/opencode` → `~/.local/share/opencode` → `%LOCALAPPDATA%\opencode`。

两边的列表都是**追加而不是覆盖**，重复的会话以先出现的为准。想只对某份拷贝操作（比如拿备份做实验），要把 `--dsh-home`、`--data-dir` 或 `USERPROFILE` 一起指到隔离目录去，否则默认位置仍会被扫描。

## 写操作的安全边界

- **DST 日志是权威数据**。重命名只追加一帧，从不重写历史；改工作区会重写 header 并移动目录，新头部先在内存里造好才动磁盘，失败会尽量回滚。
- **opencode 走事务**。写操作在 `IMMEDIATE` 事务里完成，行和事件日志一起提交或一起回滚；`busy_timeout` 让并发写入排队而不是直接失败。
- **dsh-tui 的派生文件尽力而为**。`session-index.json`、`session-lists/*.json`、`session_projcache*`、`last-used` / `resume.txt` / pins / mounts、`workspace.json` 会被同步更新；失败只吞掉，不中断操作。
- **删除是永久的**。DST 会话目录直接 `remove_dir_all`，工作区目录在最后一个会话被删掉后一并清理；opencode 会话把消息、片段、待办、事件一起删掉，并清掉 `storage/session_diff/<id>.json`。没有回收站，也没有 `--restore`——确认框是唯一的保险。
- **占用中不阻止**。被运行的进程挂载的会话会警告，但仍可操作。

## 多语言

界面文案是两张扁平 YAML（`locales/zh-CN.yml` 是原文，`locales/en.yml` 是译文），编译期嵌进二进制。选择存到 `<data_dir>/roster.json`；没存过就跟随系统（Windows 看 `GetUserDefaultUILanguage`，其他平台看 `LANG`）。旧版本写下的 `agent-session-manager.json` / `dst-session-manager.json` 会被自动识别并迁移。

新增文案时两边一起改：`i18n` 的测试会检查键集合一致、占位符（`%{name}`）一致。

## 它会在数据目录里留什么

| 文件 | 内容 |
| --- | --- |
| `roster.json` | 本工具的偏好，目前只有语言 |
| `roster-titles.json` | 从 DST 日志里刮出来的标题缓存（按 size + mtime 失效） |

改名前（`agent-session-manager` / `dst-session-manager`）留下的 `*-trash/` 和 `*-titles.json` 不会被读取或清理——那些旧回收站里的东西需要自己处理，新版本删除会话时不会再往里放。

## 开发

```console
$ cargo test          # 32 个测试
$ cargo clippy --all-targets
$ cargo fmt
```

模块分工：

| 模块 | 职责 |
| --- | --- |
| `paths.rs` | 目录发现与根目录优先级 |
| `zstd_frames.rs` | zstd 帧扫描 / 单帧解压 / 单帧压缩 |
| `store.rs` | 统一的数据模型（`Kind` + `Location`）、DST 加载、标题推导与缓存 |
| `opencode.rs` | SQLite 读取、重命名 / 删除 / 改工作区 |
| `mutate.rs` | 按来源分发的写操作 |
| `cache.rs` | 同步 dsh-tui 的派生缓存（尽力而为，原子写） |
| `app.rs` | 状态机与按键 |
| `ui.rs` | ratatui 渲染 |
| `i18n.rs` / `settings.rs` | 文案与偏好 |
| `main.rs` | 参数解析、CLI 子命令、TUI 主循环 |
| `build.rs` | Windows 资源：图标与版本信息 |

提交信息请用 `.gitmessage` 的格式：`git config commit.template .gitmessage` 之后 `git commit` 会自动套用模板。

## 已知限制

- 只在 Windows 上验证过；Unix 分支已实现但未实测。
- opencode 只支持已经迁移到 SQLite 的版本（1.x）。更早的 `storage/session/**/*.json` 布局没做支持。
- opencode 的 `part` 表很大（几百 MB 的库并不罕见），所以列表不统计字节数，只显示消息/事件数量。
- 改工作区时如果新目录不属于任何已知项目，会话仍挂在原项目下（不会伪造 project）。
- 不阻止修改被占用中的会话。
- 删除是永久的，没有回收站也没有 `--restore`。
