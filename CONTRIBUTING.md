# 贡献指南

## 环境要求

- Rust stable

## 构建

```bash
cargo build
```

## 测试

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

## 项目结构

```
src/
├── main.rs       # CLI 入口，命令路由
├── cli.rs        # clap 命令行参数定义
├── config.rs     # 环境变量读取、凭据管理、token 持久化
├── auth.rs       # OAuth 设备码认证（直接调用百度开放平台）
├── api.rs        # 百度网盘开放平台 API 封装
├── transfer.rs   # 上传/下载编排、加密、断点续传状态
├── crypto.rs     # 本地加解密（AES-256-GCM）
├── batch.rs      # 批量任务清单解析与执行
├── error.rs      # 错误类型定义
└── lib.rs        # 库根
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

- Release 资产只包含客户端 `baidupan-cli`
- `release-client` workflow 会把仓库的 `BAIDUPAN_APP_NAME`、`BAIDUPAN_CRYPTO_PASSPHRASE` Repository secrets 作为客户端的编译期默认值注入 Release 产物
- 终端用户直接运行 Release 客户端时，不再需要额外配置上述值；如果用户自己设置了同名环境变量，运行时环境变量仍然优先

### 本地构建发布包

```bash
scripts/release-client-local.sh v0.1.0
```

说明：

- 脚本会在存在 `.env` 时自动读取，并把 `BAIDUPAN_APP_KEY`、`BAIDUPAN_APP_SECRET`、`BAIDUPAN_APP_NAME`、`BAIDUPAN_CRYPTO_PASSPHRASE` 映射成编译期默认值
- 本地多平台构建默认覆盖：Linux x86_64、Linux ARM64、Windows x86_64、macOS x86_64、macOS ARM64
- 需要预先安装 `zig` 和 `cargo-zigbuild`，脚本会自动执行 `rustup target add`
- 产物默认输出到 `dist/<版本>/`，打包文件名为 `baidupan-<版本>-<平台>.tar.gz|zip`
