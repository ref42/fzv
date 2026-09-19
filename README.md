# fzv

`fzv` means `fuck zig version`, and you all know why. `fzv` could handle zig versions well.

```bash
fzv get dev, stable -path path/to/save/zig     # 安装zig到指定的位置，当前会安装最新的dev和stable版本
fzv get dev -j 4                # 用 4 条连接下载（默认 8，-j 1 就是单流）
fzv use 0.16.0      # 切到 0.16.0（已开的终端、IDE、cargo / zig build 立刻用新版）
fzv use dev         # 切到最新开发快照
fzv lls             # 看看本机装了哪些版本
fzv rm dev -yes                             # 删除
fzv update                                  # 升级 fzv 自己（GitHub Release）
```

**路径里的反斜杠会被某些 shell 吃掉**：bash / zsh / nushell 会把 `\P` 当转义删掉，写成单引号或正斜杠：

```bash
fzv get dev -path 'D:\PL_Collections\zig'
fzv get dev -path D:/PL_Collections/zig
```

## 安装

```powershell
# PowerShell 一行装好（下载对应架构的 release，校验 SHA-256，解压并写进用户 PATH）
irm https://github.com/ref42/fzv/releases/latest/download/install.ps1 | iex

# 装完（新开终端也行），fzv 自己再把 zig 装到你指定的目录：
fzv get dev, stable -path D:\zig
```

```bash
# 或者用 cargo 从源码安装
cargo install --git https://github.com/ref42/fzv

# 或者从 Release 下载 fzv-v*-windows-x86_64.zip，把里面的 fzv.exe 放进任意一个已在 PATH 里的目录
```

> `install.ps1` 默认装到 `%LOCALAPPDATA%\Programs\fzv`，不需要管理员权限；想换位置或从镜像装：
> `.\install.ps1 -InstallDir D:\Tools\fzv -BaseUrl https://mirror.example/fzv`

## 命令

| 命令 | 作用 |
|---|---|
| `fzv ls` | 列出可用版本（并显示 `dev` / `stable` 当前指向哪个） |
| `fzv lls` | 列出已装版本（`active` 当前、`incomplete` 安装不完整） |
| `fzv get [版本...] [-path DIR] [-j N]` | 下载安装；不给版本时交互式选择 |
| `fzv use [版本] [-path DIR]` | 激活版本；不给版本时从已装版本里选 |
| `fzv rm [版本...] [-yes]` | 删除版本 |
| `fzv path` | 显示版本目录与当前版本 |
| `fzv update [-force]` | 从 GitHub Release 升级 fzv 自身；`-force` 不管版本是否相同都重装 |
| `fzv v` / `fzv h` | 版本 / 帮助 |

## 选项（可选）

| 选项 | 作用 |
|---|---|
| `-path DIR` | 指定版本目录；不给就从 PATH 里已有的 fzv 条目推 |
| `-yes` / `-y` | 跳过确认提示（`rm`） |
| `-j N` | 每条归档的下载连接数，1–32，默认 8；`-j 1` 就是单流下载（`get`） |
| `-verbose` | 连镜像选择、校验值、解压路径这些细节一起打印 |
| `-force` | `update`：不管本地是不是最新，都重装一遍最新 release |



