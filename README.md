# 移动灵犀 · Agent2API 插件

Agent2API 的独立移动灵犀插件。支持短信登录、Token 导入与续期、模型目录、OpenAI 兼容对话、算力余额和每日活动领取。它与 WPS 灵犀不是同一个提供商。

### 登录和使用

打开侧栏「移动灵犀登录」，使用自己的手机号和短信验证码；如上游要求图形验证码，请在登录页面完成验证。也可以导入自己的灵犀云端 `access_token` 和可选的 `refresh_token`（不是 `lx_...` API Key）。账号添加后在模型管理拉取模型，并在账号列表查询余额。每日活动只处理符合上游资格的「AI每日畅想计划」，由网关签到中心触发；领取结果未知时不自动补发请求。

来源及协议参考见 [PROVENANCE.json](PROVENANCE.json)。本仓库不包含原 `lingxiApi` 的独立服务、数据库或后台页面。

## 下载与安装

- **按版本下载**：[Releases](https://github.com/wyq2822875262/agent2api-lingxi/releases)。
- **按提交下载**：[Actions](https://github.com/wyq2822875262/agent2api-lingxi/actions/workflows/build.yml) → 选择成功的运行 → Artifacts。下载后解开 Actions 的外层压缩包，再把里面的 `agent2api-…-<平台>.zip` 导入 [Agent2API](https://github.com/aimod-cc/Agent2API) 的插件管理；旁边的 `.sha256` 可用于校验。
- 包按**网关所在机器**选择：Windows x64、macOS Apple Silicon（arm64）/ Intel（x64）、Linux x64 / ARM64（GNU/glibc，CI 使用 Ubuntu 22.04）。这些包未作 Agent2API 插件签名。

## 从源码构建

推荐使用与 CI 相同的 Rust 1.93.1 和 Python 3.12。在目标平台执行，`TARGET` 替换为 `pack.json` 中的目标三元组：

```sh
TARGET=x86_64-unknown-linux-gnu
cargo test --locked -p agent2api-lingxi --target "$TARGET"
cargo build --locked -p agent2api-lingxi --target "$TARGET"
python3 scripts/test.py --target "$TARGET"
cargo build --locked --release -p agent2api-lingxi --target "$TARGET"
python3 scripts/package.py --target "$TARGET"
```

打包输出为 `dist/` 下的 ZIP 和 SHA-256 文件。测试只使用本地模拟服务，不需要真实账号；通过模拟测试不代表已与真实服务联调。登录页面另可用 Node.js 运行 `node scripts/verify-lingxi-ui.mjs` 进行离线逻辑检查。

## 维护

普通 push 自动检查并为五个平台生成构建产物；也可在 Actions 手动运行。需要发布稳定下载时，同步更新 `crates/lingxi/Cargo.toml`、`Cargo.lock` 和 `pack.json` 后推送 `v<版本号>` 标签。标签对应的五个平台都构建成功之后，流程才会发布 GitHub Release 并附上 ZIP 和校验文件。三个插件独立构建、独立发布。

许可：AGPL-3.0（见 [LICENSE](LICENSE)）。锁定的 SDK / Protocol 来源见 [UPSTREAM.md](UPSTREAM.md)。
