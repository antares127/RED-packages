# RED

RED 是一个自包含的软件包格式与配套包管理器，设计上参考 AUR/APM：包文件后缀为 `.red`，构建完成后即可直接安装，安装阶段不需要再次编译。

- `.red` 内部包含 manifest、已构建的 payload 和安装钩子
- `red -build` 从原生来源（`tar` / `tar.gz` / `tar.xz` / `tar.zst` 或目录）构建 `.red`，`--native` 则输出当前系统的 pacman 原生包（`.pkg.tar.gz`）
- 自包含应用压缩包（散置文件 + 启动脚本、无 desktop 文件）会自动整理为 `opt/<name>/` 布局并生成启动菜单项，安装后即可在菜单里看到
- `red -rebuild` 把 DEB、RPM、Arch 包和 AUR PKGBUILD 转换为 `.red`，需要对应的包管理器
- `red -bundle` 把一个目录（含子目录）里的所有 `.red` 打包为 `.ared` 合集，保持目录结构；`red -install xxx.ared` 一键安装合集内全部应用，装完启动菜单即可见
- `red` 支持安装、卸载、查询、校验、本地仓库和依赖解析

## 构建与安装

```bash
cargo build --release
# 安装二进制与桌面/右键菜单集成
sudo ./install.sh                       # 默认安装到 /usr/local
PREFIX="$HOME/.local" ./install.sh      # 免 root 安装到 ~/.local
```

如果 `cargo` 只对普通用户可见（例如通过 rustup 安装），先执行 `cargo build --release`，再运行上面的安装脚本。`make install` 等价于构建后执行 `install.sh`。

也可以使用 `cargo install --path .`，二进制会安装到 `~/.cargo/bin`（不会安装桌面集成文件）。

生成的二进制为 `target/release/red`。

## 快速开始

```bash
# 从预构建 tar 包构建
red -build hello-1.0.0-x86_64.tar.gz --description "Hello world"

# 把 DEB 转换为 RED（需要 dpkg-deb）
red -rebuild ./hello_2.0_amd64.deb

# 安装/查询/卸载
red -install ./hello-1.0.0-x86_64.red
red -list
red -info hello
red -files hello
red -remove hello
```

安装到其他根目录（常用于测试）：

```bash
red -install ./hello.red --root /tmp/red-root
red -list --root /tmp/red-root
```

当 `--root` 不是 `/` 时默认不执行安装钩子，需要时显式加 `--run-hooks`。

## `.red` 格式

外层是**未压缩的 POSIX tar**，成员固定为三类：

```
manifest.json              包元数据（JSON）
payload.tar.xz             已构建的文件树（压缩方式见 manifest.compression）
hooks/pre_install.sh       可选：安装前钩子
hooks/post_install.sh      可选：安装后钩子
hooks/pre_remove.sh        可选：卸载前钩子
hooks/post_remove.sh       可选：卸载后钩子
hooks/arch_install.sh      可选：Arch .INSTALL 脚本
```

- payload 成员名由 `manifest.payload` 指定，支持 `payload.tar`、`payload.tar.gz`、`payload.tar.xz`、`payload.tar.zst`
- `manifest.sha256` 是 payload 文件的 SHA-256，`red -verify` 会校验
- 从 tar.xz/tar.gz 构建时直接复制原压缩流，只做一次解压用于扫描文件列表和体积

### manifest.json

```json
{
  "format": 1,
  "name": "hello",
  "version": "1.0.0",
  "release": 1,
  "arch": "x86_64",
  "description": "Hello world",
  "url": "https://example.com/hello",
  "license": ["MIT"],
  "maintainer": "Someone <someone@example.com>",
  "depends": ["libc>=2.0"],
  "opt_depends": ["bash"],
  "provides": ["hello"],
  "conflicts": ["hello-legacy"],
  "replaces": [],
  "foreign_depends": [],
  "foreign_conflicts": [],
  "payload": "payload.tar.xz",
  "compression": "xz",
  "payload_size": 1024,
  "installed_size": 4096,
  "sha256": "…",
  "files": ["usr/bin/hello"],
  "hooks": {
    "post_install": "hooks/post_install.sh"
  },
  "origin": "tar",
  "built_at": 1700000000,
  "red_version": "0.1.0"
}
```

`release` 为 1 时完整版本号就是 `version`，大于 1 时为 `version-release`。

## Redfile

目录构建时可在目录内放置 `Redfile`（JSON），也可以显式指定 `--redfile`。从单个 tar/deb 包构建时，会读取该文件所在目录的 `Redfile`。

```json
{
  "name": "myapp",
  "version": "2.1.0",
  "release": 1,
  "arch": "x86_64",
  "description": "My application",
  "url": "https://example.com",
  "license": ["MIT"],
  "maintainer": "Someone <someone@example.com>",
  "depends": ["bash>=4.0"],
  "opt_depends": ["less"],
  "provides": ["myapp"],
  "conflicts": [],
  "replaces": [],
  "hooks": {
    "post_install": "hooks/post-install.sh",
    "pre_remove": "hooks/pre-remove.sh"
  },
  "payload_dir": "rootfs",
  "exclude": ["docs/build", "var/log/app.log"]
}
```

- 命令行参数优先于 Redfile
- `payload_dir`：把指定子目录作为安装根目录（例如 `rootfs/usr/bin/app` 会安装为 `/usr/bin/app`）
- `exclude`：相对 payload 根目录的路径前缀
- 目录构建默认排除顶层 `Redfile`、`Redfile.json`、`hooks/`、`.git/`、`.hg/`、`.svn/` 以及顶层 `.red` 文件

## 命令

| 命令 | 说明 |
| --- | --- |
| `red -build <tar.*\|dir>` | 从原生来源构建 `.red` |
| `red -rebuild <source>` | 从 DEB/RPM/Arch 包或 PKGBUILD 目录转换为 `.red` |
| `red -install <.red\|.ared\|name>` | 安装包或合集；给名称时从本地仓库解析 |
| `red -remove <name>` | 卸载 |
| `red -list` | 列出已安装包 |
| `red -info <name\|.red\|.ared>` | 查看元数据 |
| `red -files <name\|.red>` | 查看文件列表 |
| `red -verify <.red>` | 校验 payload checksum |
| `red -search <term>` | 搜索已配置的本地仓库 |
| `red -bundle <dir>` | 把目录里所有 `.red`（含子目录）打包为一个 `.ared` 合集，保持目录结构 |
| `red -repo add\|remove\|list [DIR]` | 管理本地仓库 |
| `red -gui [file]` | 打开图形界面，可直接操作归档或 `.red` |

常用选项：

- `--root DIR`：安装根目录，默认为 `/` 或环境变量 `RED_ROOT`
- `--nodeps`：跳过依赖检查
- `--force`：覆盖文件或重装同版本
- `--run-hooks` / `--no-hooks`：强制开启/关闭钩子

构建选项（`red -help build`）：`--name`、`--version`、`--release`、`--arch`、`--description`、`--url`、`--license`、`--maintainer`、`--depends`、`--provides`、`--conflicts`、`--replaces`、`--out`、`--redfile`。

## 依赖与版本

依赖写法：

```
bash
bash>=4.0
libfoo<=2.1
libbar=1.0
```

安装时优先用**已安装包**满足依赖（包括 `provides`）；否则在本地仓库中查找对应 `.red` 并递归安装，循环依赖会报错。

版本比较规则：

- 支持 epoch：`1:2.0` 的 epoch 为 1
- 数字段按数值比较（`1.2 < 1.10`）
- 字母段按字典序比较
- 数字段大于字母段（`1.0.1 > 1.0alpha`）
- 分隔符（`.`、`-`、`_`、`~`）不参与排序

## 钩子

可用的阶段：`pre_install`、`post_install`、`pre_remove`、`post_remove`、`arch_install`。阶段名中的 `-` 会规范化为 `_`。

通用脚本会以 `bash <script> <action>` 执行，`action` 映射为：

| 阶段 | action |
| --- | --- |
| `pre_install` | `install` |
| `post_install` | `configure` |
| `pre_remove` / `post_remove` | `remove` |

`arch_install` 是 Arch 的 `.INSTALL` 脚本，执行时会 source 脚本并调用与阶段同名的函数（如 `post_install`），函数不存在则跳过。

钩子环境变量：

- `RED_ROOT`：安装根目录
- `RED_DB`：`<root>/var/lib/red`
- `RED_PACKAGE` / `RED_NAME`：包名
- `RED_VERSION`：完整版本号
- `RED_ARCH`：架构
- `RED_ORIGIN`：来源（`tar`、`dir`、`deb`、`rpm`、`arch`、`aur`）

钩子内容会保存在 `<root>/var/lib/red/hooks/<name>/<phase>.sh`，卸载时执行已保存的版本。

## 来源转换

`red -build` 只接受原生来源：

| 来源 | 处理方式 |
| --- | --- |
| `*.tar` / `*.tar.gz` / `*.tar.xz` / `*.tar.zst` | 解压扫描元数据，原压缩流直接作为 payload |
| 目录 | 打包为 `payload.tar.gz`，读取 Redfile |

`red -rebuild` 接受外来包格式，并调用对应的包管理器：

| 来源 | 依赖工具 | 处理方式 |
| --- | --- | --- |
| `*.deb` | `dpkg-deb` | `dpkg-deb -e` 读取 control/脚本，`--fsys-tarfile` 导出文件树；`Depends` 记为 `foreign_depends`，`preinst`/`postinst`/`prerm`/`postrm` 映射为钩子 |
| `*.rpm` | `rpm`、`rpm2cpio`、`cpio` | `rpm -qp` 读取元数据，`rpm2cpio \| cpio` 解包文件树；依赖记为 `foreign_depends`，暂不导入 RPM 脚本 |
| `*.pkg.tar.*` | 无 | 读取 `.PKGINFO` 与 `.INSTALL`，安装时跳过 Arch 元数据文件 |
| 含 `PKGBUILD` 的目录 | `makepkg`（base-devel） | 执行 `makepkg -f --noconfirm`，再用 `--packagelist` 获取产物并转换 |
| `*.tar.*` / 目录 | 无 | 与 `-build` 相同 |

`red -rebuild` 在缺少工具时会直接报错并提示安装对应包管理器。

`foreign_depends` / `foreign_conflicts` 仅作信息保留，不参与 RED 依赖解析。

构建输出默认命名为 `<name>-<full_version>-<arch>.red`，可用 `--out` 指定。

## 安装布局

```
/
├── var/lib/red/
│   ├── installed/<name>.json   已安装记录
│   ├── hooks/<name>/<phase>.sh 已保存钩子
│   ├── repos.json              本地仓库列表
│   └── var/tmp/red/            临时工作目录
└── usr/...                     包文件
```

安装使用临时 staging 目录，先校验全部路径再提交；文件冲突、钩子失败或写入失败时会回滚本次创建的文件/目录。

## 桌面集成

安装或卸载完成后，`red` 会自动刷新桌面数据库，行为与原生包管理器一致：

- `update-desktop-database <root>/usr/share/applications`
- `gtk-update-icon-cache -f -t <root>/usr/share/icons/hicolor`
- `update-mime-database <root>/usr/share/mime`

对应目录不存在或工具未安装时会静默跳过，不影响安装结果。包只需按标准布局提供文件：

```
usr/share/applications/<app>.desktop
usr/share/icons/hicolor/<size>/apps/<icon>.png
```

`Exec` 建议使用已安装到 `usr/bin` 的命令，不要写绝对路径。

## 图形界面

`red -gui [file]` 会打开一个原生桌面窗口（不是浏览器页面），外观沿用 DMS / Material 3 卡片：大表情、`当前文件：…`、胶囊操作按钮和底部进度线。

- 颜色与圆角在运行时读取系统主题（`~/.cache/DankMaterialShell/dms-colors.json`），深浅色跟随当前桌面，没有写死的白色或固定配色
- 界面由 QML 绘制，运行在 `quickshell` 上；窗口是普通桌面窗口，可被移动/平铺，不会遮挡 pkexec 授权弹窗
- 传入 `.tar.*` / 目录：可构建 `.red`，或构建适用当前系统的 pacman 原生包（`.pkg.tar.gz`）
- 传入 `.deb` / `.rpm` / `.pkg.tar.*` / 含 PKGBUILD 的目录：转换为 `.red`
- 传入 `.red`：可直接安装；非 root 运行时通过 `pkexec` 弹出授权窗口
- 传入 `.ared`：显示合集内全部包，一键安装（已安装的同名同版本自动跳过）
- 传入目录：若目录内含 `.red` 包，额外提供「构建 ARED 合集包」
- 构建/安装命令由 QML 直接调用当前 `red` 可执行文件，输出实时显示在“细节”面板
- `Esc` 或右上角 `✕` 关闭窗口，点击“细节 ›”展开输出

QML 源码在 `gui/shell.qml`，编译时嵌入 `red` 二进制，运行时写入 `$XDG_RUNTIME_DIR/red-gui-<pid>/` 后交给 quickshell 加载，因此不依赖安装路径。

需要安装：

```bash
sudo pacman -S quickshell    # 或 quickshell-git，DankMaterialShell 已自带
```

缺少 quickshell 时 `red -gui` 会直接提示，不会静默失败。

`make install` 会同时安装以下集成文件：

```
share/applications/red.desktop                          # 应用菜单入口与 application/x-red 关联
share/kio/servicemenus/red-servicemenu.desktop          # KDE 文件管理器右键“使用 RED 打开”
share/mime/packages/red.xml                             # .red MIME 类型
share/icons/hicolor/scalable/apps/red.svg               # 图标
```

安装完成后，在 Dolphin 中右键 `tar.gz`、`.red`、`.deb`、`.rpm` 等文件即可看到“使用 RED 打开”。如果菜单未立即出现，注销重登或运行 `kbuildsycoca6`。

## 限制

- 只支持本地 `.red` 文件与本地仓库目录，没有远程仓库索引
- 不做签名校验，只校验 payload SHA-256
- 依赖系统理解的运行时（如动态链接库）需要在 Redfile/包元数据中显式声明
- AUR 构建依赖本机已安装 `makepkg` 与 `base-devel`
- 安装到 `/` 通常需要 root 权限；图形界面的安装操作依赖 `pkexec`
- 图形界面依赖 `quickshell`（DankMaterialShell 已自带）

## 开发

```bash
cargo build
cargo test
```

单元测试覆盖版本比较与 xz/gzip payload 解压。
