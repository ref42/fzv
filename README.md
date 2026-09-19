# fzv

Windows 上的 Zig 版本管理：**`PATH` 指向哪个版本，哪个版本就是当前版本**——切版本即时生效，不用重启终端。

```powershell
fzv use 0.16.0      # 切到 0.16.0（已开的终端、IDE、cargo / zig build 立刻用新版）
fzv use dev         # 切到最新开发快照
fzv lls             # 看看本机装了哪些版本
```

除了你指定的版本目录，fzv 不在别处留文件（不写 `%LOCALAPPDATA%`）。

## 安装

```powershell
cargo build --release     # 产物 target\release\fzv.exe，放进 PATH 即可
```

> 仓库里的 `.cargo/config.toml` 带一个 nightly 专用参数，所以要用 nightly 工具链构建；想用 stable 就删掉那个文件。

## 上手

```powershell
# 第一次：告诉 fzv 版本装到哪儿（以后都不用再写路径）
fzv get stable --path D:\PL_Collections\zig

fzv lls                                        # 已装版本，active 就是当前用的
fzv use 0.17.0-dev.2228+955228b68              # 切版本
fzv path                                       # 版本目录 / 当前版本
fzv rm 0.15.2 --yes                            # 删除
fzv update                                     # 升级 fzv 自己（GitHub Release）
```

`fzv get <版本> --path DIR` 装完会顺手激活，不用再 `use` 一次。

## 命令

| 命令 | 作用 |
|---|---|
| `fzv ls` | 列出可用版本（并显示 `dev` / `stable` 当前指向哪个） |
| `fzv lls` | 列出已装版本（`active` 当前、`incomplete` 安装不完整） |
| `fzv get [版本...] [--path DIR]` | 下载安装；不给版本时交互式选择 |
| `fzv use [版本] [--path DIR]` | 激活版本；不给版本时从已装版本里选 |
| `fzv rm [版本...] [--yes]` | 删除版本 |
| `fzv path` | 显示版本目录与当前版本 |
| `fzv update` | 从 GitHub Release 升级 fzv 自身 |
| `fzv update --force` | 不管本地是不是最新，都重新装一遍 GitHub 上最新的 release |
| `fzv v` / `fzv h` | 版本 / 帮助 |

选择器：`dev`（最新开发快照）、`stable`（最新稳定版）、`latest`（同 `dev`）、精确版本（`0.16.0`）。

一次可以给多个，用空格或逗号分隔都行：`fzv get dev stable` ≡ `fzv get dev,stable`。

## 必读的三点

**1. 切版本不用重启终端。** `PATH` 里只有一条稳定条目 `<版本目录>\.fzv\bin`，`fzv use` 只改其中一个小文件，所以任何已经开着的终端、编辑器、构建工具，下一次调用 `zig` 就是新版本。
（`zig` / `zls` 是 fzv 自己的转发程序，所以 `where zig` 显示的是 `.fzv\bin\zig.exe`，这是正常的。）

**2. 装完立刻就能用，不用接 PATH。** fzv 把 `zig.exe` / `zls.exe` 两个转发程序放在 `fzv.exe` 自己所在的目录（那个目录本来就在你的 PATH 里），所以 `fzv get` / `fzv use` 之后，**包括你当前开着的这个终端**，`zig` 马上就能用；`fzv use` 切版本也是下一次调用 `zig` 就生效（只改一个小文件），不会再去动 PATH。

例外只有一种：`fzv.exe` 所在目录不在 PATH 里、或不可写（比如放在只读位置）。这时 fzv 会提醒一次，照下面任一种方式接一次即可 —— 只要一次，之后永远不用再管（进程无法修改父 shell 的环境变量，所以这一步只能由 shell 自己做）：

```powershell
$env:PATH = (fzv use 0.16.0 --print-path)          # PowerShell
export PATH="$(fzv use 0.16.0 --print-path)"        # bash / zsh
```
```bat
for /f "delims=" %p in ('fzv use 0.16.0 --print-path') do set "PATH=%p"   :: cmd
```

嫌麻烦可以在 `$PROFILE` 里加个包装函数，让 `fzv use` 自动接上：

```powershell
function fzv {
    $exe = (Get-Command fzv.exe).Source
    if ($args.Count -gt 0 -and $args[0] -eq 'use') { $env:PATH = & $exe @args --print-path }
    else { & $exe @args }
}
```

**3. 路径里的反斜杠会被某些 shell 吃掉。** bash / zsh / nushell 会把 `\P` 当转义删掉，写成单引号或正斜杠：

```bash
fzv get dev --path 'D:\PL_Collections\zig'
fzv get dev --path D:/PL_Collections/zig
```

## fzv 写了哪些文件

```
D:\PL_Collections\zig\          ← 你指定的版本目录
├─ 0.16.0\ …                    各版本（zig.exe 直接在版本目录里）
├─ zls\                         共享的 ZLS
└─ .fzv\
   ├─ active                    当前版本（一行文本）
   ├─ bin\                      zig.exe / zls.exe（转发程序，PATH 里那一条）
   ├─ download-index.json       下载索引缓存
   ├─ locks\                    安装锁
   └─ session-hint              已提醒过「这个终端要接一次 PATH」的标记

D:\RUST\.cargo\bin\             ← fzv.exe 所在目录（你本来就在用它）
├─ fzv.exe
├─ zig.exe                     ┐ 同两个转发程序也放一份在这里：这个目录
└─ zls.exe                     ┘ 已在 PATH 里，所以当前终端立刻就能用
```
fzv 写的东西只有上面这些；唯一改动的系统设置，是用户 PATH 里那一条 shim 目录。
另外各版本目录下可能出现 `*.downloading` 和 `*.downloading.chunks`，那是没下完的归档和它的续传记录，装完会自动删掉。

## 环境变量（可选）

| 变量 | 作用 |
|---|---|
| `FZV_MIRROR` | 只用指定镜像 |
| `FZV_MIRRORS` | 指定要尝试的镜像列表（逗号分隔，替换内置列表） |
| `FZV_NO_MIRRORS` | 只用 ziglang.org |
| `FZV_DOWNLOAD_JOBS` | 分段下载并发数（默认 8） |
| `FZV_REFRESH_INDEX` | 忽略索引缓存 |
| `FZV_VERBOSE` | 连镜像选择、校验、解压这些细节一起打印 |
| `FZV_REPO` | `fzv update` 从别的仓库升级（默认 `ref42/fzv`） |
| `FZV_RELEASES_URL` | `fzv update` 从 GitHub Enterprise / 自建服务器升级 |

默认只打印该管的事：下载进度条、装了什么、当前是哪个版本、以及出错信息。进度条形如 `[========>-------] 19.09 MiB/52.30 MiB 1.01 MiB/s`（进度 + 已下载/总体积 + 实时速度），在第一个字节到达后才出现（不会先摆一行 `0 B 0 B/s` 占着），只在终端里画，重定向到文件时整行都自动不画；几条 KB 的内部下载（下载索引）不显示进度。想看 fzv 到底在干什么（选了哪个镜像、校验值、解压路径），加 `FZV_VERBOSE=1`。

默认并行探测 16 个社区镜像并挑最快的。**镜像不是"选中一个就一路用到底"**：它们按实测速度排成一个列表，某个镜像开始拒绝请求（公开镜像上很常见的 `429 Too Many Requests`）会**立刻换下一个**继续下，已经下到的部分保留、从断点继续；一个都不行才会报错。Zig 归档一律用官方索引里的 SHA-256 校验，不匹配就重下；ZLS 官方没有校验和，只提示不校验。

大文件（≥ 4 MiB）会切成小块，由 8 条连接抢着下载，所以不会出现「某条连接拖后腿、最后一段特别慢」的情况；进度条上的速度是最近几秒的实时速度，不是从头开始的平均值。中断的下载会记下已完成的块，下次接着下。

## 常见问题

| 现象 | 处理 |
|---|---|
| `zig` 找不到 | 新开一个终端；当前终端用 `$env:PATH = (fzv use <版本> --print-path)` 接一次 |
| 装机后当前终端仍找不到 `zig` | 说明 `fzv.exe` 不在 PATH 里或所在目录不可写：把它放到 PATH 里的目录（比如 `cargo install --path .` 装出来的位置），再跑一次 `fzv use` |
| `no versions directory is known` | PATH 里还没有 fzv 条目：给一次 `--path DIR` |
| `no active Zig version recorded in …` | 当前版本被删了：`fzv use <版本>` |
| 某个版本显示 `incomplete` | 上次安装被中断：重跑 `fzv use <版本>` 自动修复 |
| 想换一个版本目录 | `fzv use <版本> --path 新目录` |
| 想升级 ZLS | 删掉 `<版本目录>\zls`，再跑一次 `fzv use` |
| 下载速度忽快忽慢 | 先看进度条：它显示的是当前实际速度。若长时间只有几百 KiB/s 而镜像探测时很快，多半是该镜像在限速，用 `FZV_NO_MIRRORS=1` 换回 ziglang.org 试试 |
| `fzv update` 说找不到 release | 仓库里还没有 Release：需要先打一个 `v<Cargo.toml 里的版本>` 标签并推送，由 `.github/workflows/deploy.yml` 自动构建发布 |
| `fzv update` 说已是最新，但我想重装 | `fzv update --force`：不管版本号，直接装 GitHub 上最新的那个 release（校验照做），适合修复装坏的 fzv 或从自己编译的版本回到发布版 |
| `fzv update` 和 `cargo install` 冲突吗 | 两者都写同一个 `fzv.exe`：`cargo install --force` 会覆盖 `fzv update` 装的，反之亦然。用其中一个就好 |
