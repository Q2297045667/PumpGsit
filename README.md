# pumpgsit

在 Minecraft Java 26.3 中，玩家可以坐在台阶、楼梯或另一位玩家身上，也可以躺下、翘腿、趴下、旋转和爬行。pumpgsit 从 Gecolay 的 Bukkit/Paper 插件 GSit 3.8.0 迁移，主分支使用 Rust 目标 `wasm32-wasip3` 构建，插件接口为 `pumpkin:plugin@0.2.0`。

## 下载

- **正式版**按版本标签发布。生产服可从最新正式版下载插件。[下载最新正式版 pumpgsit.wasm](https://github.com/Q2297045667/pumpgsit/releases/latest/download/pumpgsit.wasm) · [正式版发布说明](https://github.com/Q2297045667/pumpgsit/releases/latest)
- **每夜构建**计划于北京时间每日 02:17 从默认分支生成。打开每夜发布列表，选择所需日期及提交的预发布版，从附件下载同名插件。服务端插件列表会显示形如 `0.1.1-nightly.YYYYMMDD+提交短号` 的版本。[每夜发布列表](https://github.com/Q2297045667/pumpgsit/releases?q=nightly) · [工作流运行记录](https://github.com/Q2297045667/pumpgsit/actions/workflows/release.yml)

使用 WASI 0.3 时，请选择 `v0.1.1` 或对应的每夜构建。`v0.1.0` 和首个每夜版（提交 `4a5cbbf`）使用 `wasm32-wasip2`。

正式版与每夜版均提供 `pumpgsit.wasm` 和 `SHA256SUMS`。将两份文件下载到同一目录后执行 `sha256sum -c SHA256SUMS`，应显示 `pumpgsit.wasm: OK`。

## 安装

1. 使用启用 `plugin-v0_2` 的 Pumpkin。目标服务端是 `0.2.2+26.3-26.63`，提交 `e318ec5ff87bbd0d72feafe40d6b24f230a8b84a`，Java 客户端为 **26.3（协议 777）**。从该提交构建服务端时运行 `cargo build --locked --release -p pumpkin --features plugin-v0_2`。
2. 停服，将 `pumpgsit.wasm` 放入服务端的 `plugins/`。若已使用旧名 `gsit.wasm` 的 Pumpkin 插件，先按迁移说明备份并迁移其数据、权限，再替换插件文件。[旧 Pumpkin 版或 Bukkit 版迁移](doc/存储.md)
3. 在 `pumpkin.toml` 中为插件开放自己的数据目录。发布的 Wasm 为未签名组件，本例仅对 pumpgsit 放行。

   ```toml
   [plugins.overrides.pumpgsit]
   allow_unsigned = true
   allowed_permissions = ["fs.read.data", "fs.write.data"]
   ```

4. 启动后，检查控制台有 `Loaded pumpgsit` 及所装版本号。输入 `pumpgsitreload`，默认英文回复为 `pumpgsit configuration reloaded.`。玩家可站在完整方块上执行 `/sit`，在第三人称观察弯膝坐姿，再按潜行键起身。
5. 配置文件位于 `plugins/data/pumpgsit/config.toml`，玩家偏好位于 `plugins/data/pumpgsit/data/data.db`。

## 文档

- [命令、别名、权限与 TOML 配置](doc/使用与配置.md)
- [六个占位符与可选联动](doc/占位符.md)
- [旧库备份、迁移、核验与回滚](doc/存储.md)
- [构建、发布、功能对照与已知差异](doc/构建与验证.md)
- [Java 客户端验收与截图](doc/客户端验证.md)
- [姿势实现与协议验证](doc/姿势.md)

原版作者为 Gecolay。pumpgsit 源码采用 GPL-3.0-only，随附的 Pumpkin WIT 保留了各自的许可证文件。[原版 GSit 3.8.0 源码](https://github.com/gecolay/GSit/tree/04b792026acf95b63f198691d85218a1023e2f92) · [插件许可证](LICENSE) · [WIT 与许可证](wit/)
