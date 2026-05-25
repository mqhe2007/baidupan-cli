# baidupan-cli

基于百度网盘开放平台 API，使用 Rust 编写的百度网盘终端客户端。

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

当前仍在开发：

- 批量任务

## 环境要求

- Rust stable
- 百度网盘开放平台应用 `AppKey` 和 `SecretKey`

## 环境变量

- `BAIDUPAN_APP_KEY`: 百度开放平台 AppKey
- `BAIDUPAN_APP_SECRET`: 百度开放平台 SecretKey
- `BAIDUPAN_CRYPTO_PASSPHRASE`: 可选。上传加密或下载解密时优先读取；未设置时会在终端交互输入

## 构建

```bash
cargo build
```

## 登录

```bash
export BAIDUPAN_APP_KEY=your_app_key
export BAIDUPAN_APP_SECRET=your_app_secret

cargo run -- login
```

登录后程序会输出：

- 授权地址 `verification_url`
- 用户码 `user_code`
- 二维码地址 `qrcode_url`

授权完成后，token 会保存在系统配置目录下的 `baidupan-cli/tokens.json`。

## 目录与文件命令

```bash
cargo run -- whoami
cargo run -- ls /
cargo run -- mkdir /apps/demo
cargo run -- rm /apps/demo/file.txt
cargo run -- mv /apps/demo/a.txt /apps/demo/b.txt
cargo run -- cp /apps/demo/b.txt /apps/demo/c.txt
```

`ls` 支持 `--json` 输出：

```bash
cargo run -- --json ls /
```

## 上传与下载

上传使用百度网盘的 `precreate -> superfile2 -> create` 链路；下载通过 `filemetas` 获取 `dlink` 后流式写入本地文件。

```bash
cargo run -- upload ./local.txt /apps/demo/local.txt --encrypt
cargo run -- download /apps/demo/local.txt ./local.txt --decrypt
cargo run -- download /apps/demo/local.txt ./local.txt --force
```

说明：

- `--encrypt` 会先在本地加密，再把密文上传到网盘。
- `--decrypt` 适用于下载由本客户端加密上传的文件。
- `download` 默认不会覆盖已存在的目标文件，覆盖时使用 `--force`。
- 上传分片主机按官方 `locateupload` 接口动态获取，并优先使用返回的 `https` 域名。
- 上传中断后，重新执行同一条 `upload` 命令会复用上一次的 `uploadid`，并按 `precreate` 返回的剩余分片继续上传。
- 加密上传在续传期间会缓存本地密文；上传成功后会自动清理缓存和续传状态。
- 下载中断后，重新执行同一条 `download` 命令会从本地 `.目标文件名.baidupan.part` 继续续传。
- 下载续传会同时维护 `.目标文件名.baidupan.resume.json` 侧边状态文件；下载成功后会自动清理。
- 当前上传默认不覆盖远端同名文件；若远端已存在，同名冲突将由接口返回错误。

## 测试

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

## 说明

- OAuth 设备码模式使用官方 `basic,netdisk` scope。
- `refresh_token` 按百度开放平台规则轮换，刷新成功后会覆盖本地旧 token。
- 目前默认只保存用户 token，不保存应用密钥。
- 当前下载路径依赖通过父目录列表解析远端路径，再通过 `filemetas` 获取 `dlink`。
