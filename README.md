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

> **注意**：本项目当前处于快速迭代期，API 和命令行行为可能会有变更。请关注 [Releases](https://github.com/mqhe2007/baidupan-cli/releases) 页面的更新日志，升级前建议查阅最新文档。

## 安装

从 [Releases](https://github.com/mqhe2007/baidupan-cli/releases) 页面下载对应平台的二进制，解压后即可使用。

从源码构建请参阅 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 环境变量

- `BAIDUPAN_APP_KEY`: 百度开放平台 AppKey。**必填**
- `BAIDUPAN_APP_SECRET`: 百度开放平台 SecretKey。**必填**
- `BAIDUPAN_APP_NAME`: 百度开放平台申请接入时填写的产品名称。CLI 会自动把所有远端路径映射到 `/apps/<应用名>/...`。**必填**
- `BAIDUPAN_CRYPTO_PASSPHRASE`: 使用 `--encrypt` 或 `--decrypt` 时**必填**。未设置时会报错中断，避免遗忘密码导致数据无法解密

## 登录

```bash
export BAIDUPAN_APP_KEY=your_app_key
export BAIDUPAN_APP_SECRET=your_app_secret
export BAIDUPAN_APP_NAME=your_product_name

baidupan-cli login
```

登录后程序会输出：

- 授权地址 `verification_url`
- 用户码 `user_code`
- 二维码地址 `qrcode_url`

授权完成后，token 会保存在系统配置目录下的 `baidupan-cli/tokens.json`。

## 目录与文件命令

```bash
baidupan-cli whoami
baidupan-cli ls /
baidupan-cli mkdir demo
baidupan-cli rm demo/file.txt
baidupan-cli mv demo/a.txt demo/b.txt
baidupan-cli cp demo/b.txt demo/c.txt
```

`ls` 支持 `--json` 输出：

```bash
baidupan-cli --json ls /
```

## 上传与下载

上传使用百度网盘的 `precreate -> superfile2 -> create` 链路；下载通过 `filemetas` 获取 `dlink` 后流式写入本地文件。

```bash
baidupan-cli upload ./local.txt demo/local.txt --encrypt
baidupan-cli download demo/local.txt ./local.txt --decrypt
baidupan-cli download demo/local.txt ./local.txt --force
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
baidupan-cli batch ./tasks.json
baidupan-cli batch ./tasks.json --continue-on-error
baidupan-cli --json batch ./tasks.json
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

## 说明

- OAuth 设备码模式使用官方 `basic,netdisk` scope。
- `refresh_token` 按百度开放平台规则轮换，刷新成功后会覆盖本地旧 token。
- 目前默认只保存用户 token，不保存应用密钥。
- 当前下载路径依赖通过父目录列表解析远端路径，再通过 `filemetas` 获取 `dlink`。

## 贡献

请参阅 [CONTRIBUTING.md](CONTRIBUTING.md)。
