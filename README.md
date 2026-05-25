# baidupan-cli

基于百度网盘开放平台 API，使用 Rust 编写的百度网盘终端客户端。

## 项目信息

- 作者：孟庆贺
- 主页：https://mengqinghe.com
- 仓库：https://github.com/mqhe2007/baidupan-cli
- 许可证：Apache License 2.0

当前已实现：

- OAuth 设备码登录
- 本地 token 持久化与自动刷新
- 目录列表
- 创建目录
- 文件删除、移动、复制
- 真实上传流程（precreate、分片上传、create）
- 真实下载流程（filemetas、dlink）
- 上传前本地加密
- 下载后本地解密
- 上传与下载进度条
- 上传与下载失败时的上下文错误提示
- 上传断点续传
- 下载断点续传
- 批量任务（JSON 清单顺序执行）

## 环境要求

- Rust stable
- 百度网盘开放平台应用 `AppKey`、`SecretKey` 和应用目录名
- 面向终端用户发布时，建议额外部署一个认证后端二进制，由它代持 `AppKey/SecretKey`

## 环境变量

- `BAIDUPAN_APP_KEY`: 百度开放平台 AppKey。开发或直连认证模式需要
- `BAIDUPAN_APP_SECRET`: 百度开放平台 SecretKey。开发或直连认证模式需要
- `BAIDUPAN_APP_NAME`: 百度开放平台申请接入时填写的产品名称。CLI 会自动把所有远端路径映射到 `/apps/<应用名>/...`
- `BAIDUPAN_AUTH_SERVER`: 可选。认证后端地址，例如 `https://auth.example.com`。设置后，CLI 的登录和 token 刷新会走你的后端，不再要求终端用户本地提供 `BAIDUPAN_APP_SECRET`
- `BAIDUPAN_CRYPTO_PASSPHRASE`: 可选。上传加密或下载解密时优先读取；未设置时会在终端交互输入

## 构建

```bash
cargo build
```

## 登录

开发或本地直连百度 OAuth：

```bash
export BAIDUPAN_APP_KEY=your_app_key
export BAIDUPAN_APP_SECRET=your_app_secret
export BAIDUPAN_APP_NAME=your_product_name

cargo run -- login
```

发布给终端用户时，推荐改成：

```bash
export BAIDUPAN_APP_NAME=your_product_name
export BAIDUPAN_AUTH_SERVER=https://auth.example.com

cargo run -- login
```

登录后程序会输出：

- 授权地址 `verification_url`
- 用户码 `user_code`
- 二维码地址 `qrcode_url`

授权完成后，token 会保存在系统配置目录下的 `baidupan-cli/tokens.json`。

## 认证后端

仓库同时提供一个认证后端二进制：`baidupan-auth-server`。它只负责三件事：

- 申请设备码
- 轮询设备码换取 token
- 使用 refresh token 刷新 access token

它不代理上传、下载或目录操作，所以不会承载文件流量。

构建：

```bash
cargo build --release --bin baidupan-auth-server
```

运行：

```bash
export BAIDUPAN_APP_KEY=your_app_key
export BAIDUPAN_APP_SECRET=your_app_secret
export BAIDUPAN_APP_NAME=your_product_name
export BAIDUPAN_AUTH_SERVER_BIND=0.0.0.0:28681

./target/release/baidupan-auth-server
```

如果你在构建时通过环境变量提供了 `BAIDUPAN_DEFAULT_APP_KEY`、`BAIDUPAN_DEFAULT_APP_SECRET`、`BAIDUPAN_DEFAULT_APP_NAME`，服务端二进制也会把它们作为编译期默认值嵌入；运行时同名环境变量仍然优先。

CLI 指向它：

```bash
export BAIDUPAN_APP_NAME=your_product_name
export BAIDUPAN_AUTH_SERVER=http://your-server:28681

./target/release/baidupan-cli login
```

说明：

- 认证后端必须部署在你控制的机器上，不要随 CLI 一起分发给终端用户。
- 终端用户侧不再需要 `BAIDUPAN_APP_SECRET`。
- 认证后端当前只转发 OAuth 相关能力，现有文件上传下载仍由 CLI 直接调用百度网盘开放平台。

## 目录与文件命令

```bash
cargo run -- whoami
cargo run -- ls /
cargo run -- mkdir demo
cargo run -- rm demo/file.txt
cargo run -- mv demo/a.txt demo/b.txt
cargo run -- cp demo/b.txt demo/c.txt
```

`ls` 支持 `--json` 输出：

```bash
cargo run -- --json ls /
```

## 上传与下载

上传使用百度网盘的 `precreate -> superfile2 -> create` 链路；下载通过 `filemetas` 获取 `dlink` 后流式写入本地文件。

```bash
cargo run -- upload ./local.txt demo/local.txt --encrypt
cargo run -- download demo/local.txt ./local.txt --decrypt
cargo run -- download demo/local.txt ./local.txt --force
```

说明：

- 远端路径参数都是相对于应用目录的路径；`/` 表示当前应用的根目录，也就是 `/apps/$BAIDUPAN_APP_NAME`。
- `upload` 传入的远端参数如果是 `/` 或以 `/` 结尾的目录路径，CLI 会自动追加本地文件名作为目标文件名。
- 命令行里不要手动写 `/apps/<应用名>/` 这一段，CLI 会自动补齐。
- `--encrypt` 会先在本地加密，再把密文上传到网盘。
- `--decrypt` 适用于下载由本客户端加密上传的文件。
- `download` 默认不会覆盖已存在的目标文件，覆盖时使用 `--force`。
- 上传分片主机按官方 `locateupload` 接口动态获取，并优先使用返回的 `https` 域名。
- 上传中断后，重新执行同一条 `upload` 命令会复用上一次的 `uploadid`，并按 `precreate` 返回的剩余分片继续上传。
- 加密上传在续传期间会缓存本地密文；上传成功后会自动清理缓存和续传状态。
- 下载中断后，重新执行同一条 `download` 命令会从本地 `.目标文件名.baidupan.part` 继续续传。
- 下载续传会同时维护 `.目标文件名.baidupan.resume.json` 侧边状态文件；下载成功后会自动清理。
- 当前上传默认不覆盖远端同名文件；若远端已存在，同名冲突将由接口返回错误。

## 批量任务

`batch` 子命令读取一个 JSON 清单，按顺序执行当前已支持的任务类型：`mkdir`、`rm`、`mv`、`cp`、`upload`、`download`。

```bash
cargo run -- batch ./tasks.json
cargo run -- batch ./tasks.json --continue-on-error
cargo run -- --json batch ./tasks.json
```

清单既可以是任务数组，也可以是带 `tasks` 字段的对象。示例：

```json
[
	{
		"type": "mkdir",
		"path": "demo"
	},
	{
		"type": "upload",
		"local": "./local.txt",
		"remote": "demo/local.txt",
		"encrypt": true
	},
	{
		"type": "download",
		"remote": "demo/local.txt",
		"local": "./downloads/local.txt",
		"decrypt": true,
		"force": true
	}
]
```

说明：

- 默认遇到首个失败任务即停止；加 `--continue-on-error` 后会继续执行后续任务，并在最后汇总失败项。
- 清单里的远端 `path`、`from`、`to`、`remote` 字段也都是相对于应用目录的路径。
- `upload` 与 `download` 在批量模式下仍保留当前的续传、加解密和进度展示行为。
- `--json` 会输出整批任务的执行汇总，适合脚本调用。

## 测试

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

## 发布

仓库已配置 GitHub Actions：

- `ci`: 在 `main` 分支推送和 Pull Request 上执行 `cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings`
- `release-client`: 在推送 `v*` tag 时，构建并发布客户端二进制 `baidupan-cli`，覆盖 Linux x86_64、Linux ARM64、Windows x86_64、macOS x86_64、macOS ARM64

发布客户端：

```bash
git tag v0.1.0
git push origin v0.1.0
```

说明：

- Release 资产只包含客户端 `baidupan-cli`，不会打包 `baidupan-auth-server`
- 认证后端二进制仍由你自己单独构建和部署
- `release-client` workflow 会把仓库的 `BAIDUPAN_APP_NAME`、`BAIDUPAN_AUTH_SERVER`、`BAIDUPAN_CRYPTO_PASSPHRASE` Repository secrets 作为客户端的编译期默认值注入 Release 产物
- 终端用户直接运行 Release 客户端时，不再需要额外配置上述三个值；如果用户自己设置了同名环境变量，运行时环境变量仍然优先

本地构建一套多平台正式发行包：

```bash
scripts/release-client-local.sh v0.1.0
```

说明：

- 脚本会在存在 `.env` 时自动读取，并把 `BAIDUPAN_APP_KEY`、`BAIDUPAN_APP_SECRET`、`BAIDUPAN_APP_NAME`、`BAIDUPAN_AUTH_SERVER`、`BAIDUPAN_CRYPTO_PASSPHRASE` 映射成编译期默认值
- 本地脚本会一起打包 `baidupan-cli` 和 `baidupan-auth-server`，适合开发者或你自己的业务交付场景
- 本地多平台构建默认覆盖：Linux x86_64、Linux ARM64、Windows x86_64、macOS x86_64、macOS ARM64
- 需要预先安装 `zig` 和 `cargo-zigbuild`，脚本会自动执行 `rustup target add`
- 产物默认输出到 `dist/<版本>/`，打包文件名为 `baidupan-toolkit-<版本>-<平台>.tar.gz|zip`
- 本地打包时 `BAIDUPAN_AUTH_SERVER` 是可选项；如果未提供，打出来的客户端只是没有内置认证后端默认地址
- 用本地脚本打出的 `baidupan-auth-server` 现在也会带上 `.env` 里的 AppKey、AppSecret、AppName 默认值；部署时可以不再额外设置这三项，除非你想在运行时覆盖它们

## 说明

- OAuth 设备码模式使用官方 `basic,netdisk` scope。
- `refresh_token` 按百度开放平台规则轮换，刷新成功后会覆盖本地旧 token。
- 目前默认只保存用户 token，不保存应用密钥。
- 当前下载路径依赖通过父目录列表解析远端路径，再通过 `filemetas` 获取 `dlink`。
