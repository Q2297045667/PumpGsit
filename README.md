# PumpGsit

PumpGsit 将 GSit 3.8.0 的坐下、玩家叠坐、仰卧、翘腿、俯卧、旋转和爬行带到 Pumpkin。插件面向 Minecraft Java 26.3（协议 777），使用 `pumpkin:plugin@0.2.0` 接口，构建目标为 `wasm32-wasip3`。玩家的三项动作偏好可沿用原版 GSit 的 SQLite 文件或 MySQL 数据库。已发布的 v0.1.3 正式版支持 SQLite。使用 MySQL 时请选择包含数据库配置功能的每夜版。

## 下载与安装

1. 从最新正式版下载 `PumpGsit.wasm` 和 `SHA256SUMS`。如要测试某次推送的改动，可按提交号选择对应的每夜版。在下载目录执行 `sha256sum -c SHA256SUMS`，确认输出 `PumpGsit.wasm: OK`。[最新正式版](https://github.com/Q2297045667/PumpGsit/releases/latest) · [每夜版](https://github.com/Q2297045667/PumpGsit/releases?q=nightly)
2. 停服，将 `PumpGsit.wasm` 放入服务端的 `plugins/`。目标 Pumpkin 版本为 `0.2.2+26.3-26.63`（提交 `e318ec5f`），构建服务端时须启用 `plugin-v0_2`。从 Bukkit 或旧名 `pumpgsit`、`gsit` 升级时，先备份并核对数据目录、三项默认值和玩家权限。[旧服迁移与回滚](doc/存储.md)
3. 在服务端的 `pumpkin.toml` 中允许加载此未签名组件，并开放插件数据目录。

   ```toml
   [plugins.overrides.PumpGsit]
   allow_unsigned = true
   allowed_permissions = ["fs.read.data", "fs.write.data"]
   blocked_permissions = ["network.tcp.connect", "network.dns"]
   ```

   上例适用于 SQLite。使用 MySQL 时，将 `network.tcp.connect` 和 `network.dns` 从 `blocked_permissions` 移入 `allowed_permissions`，在插件 `config.toml` 的 `[database]` 中填写连接参数。[MySQL 配置、迁移与回滚](doc/存储.md#mysql-配置与切换)

4. 启动后确认控制台出现 `Loaded PumpGsit` 和所装版本号。玩家站在完整方块上执行 `/sit`，在第三人称下可看到屈膝坐姿，按潜行键起身。默认配置、语言文件和默认 SQLite 库分别位于 `plugins/data/PumpGsit/` 下的 `config.toml`、`lang/` 和 `data/data.db`。

## 使用与维护

| 我想要…… | 文档 |
| --- | --- |
| 设置方块座位、动作权限或玩家语言 | [命令、别名、权限与 TOML 配置](doc/使用与配置.md) |
| 迁移或回滚 Bukkit 与旧 Pumpkin 插件的数据 | [存储、备份和回滚](doc/存储.md) |
| 在聊天或侧边栏显示坐姿与偏好 | [六个占位符及单独安装 PumpkinPIE 的方法](doc/占位符.md) |
| 核对原版行为、退出重连坐标或 SQLite 往返结果 | [Paper 与 Pumpkin 实服对照](doc/原版兼容性审计.md) |
| 构建、运行测试或检查部署限制 | [构建、功能对照与验证](doc/构建与验证.md) |
| 对照游戏画面或检查姿势协议 | [Java 客户端实测](doc/客户端验证.md) · [姿势实现](doc/姿势.md) |

原版作者为 Gecolay。PumpGsit 源码采用 GPL-3.0-only，随附的 Pumpkin WIT 保留各自的许可证。[原版 GSit 3.8.0 源码](https://github.com/gecolay/GSit/tree/04b792026acf95b63f198691d85218a1023e2f92) · [插件许可证](LICENSE) · [WIT 与许可证](wit/)
