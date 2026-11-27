# PumpGsit

<p align="center">
  <img src="assets/icon.png" width="256" height="256" alt="PumpGsit 插件图标">
</p>

PumpGsit 把 GSit 3.8.0 的坐姿和爬行带到了 Pumpkin。
玩家可以坐在台阶上、叠坐在朋友头上，也可以躺下、翘腿、趴下或旋转。

- **服务端**需要使用 Pumpkin `0.2.2+26.3-26.63` 提交 `9b4c00eab`，构建时开启 `plugin-v0_2`。
- **可选依赖**：
  - PumpkinPIE - 需要占位符时安装，测过 0.2.7。[占位符与依赖](doc/占位符.md)

## 下载与安装

1. 下载与服务端配套的 `PumpGsit.wasm` 和同次发布的 `SHA256SUMS`，或从当前源码构建。在这两个文件所在的目录执行 `sha256sum -c SHA256SUMS`，确认 `PumpGsit.wasm: OK`。[正式版](https://github.com/Q2297045667/PumpGsit/releases/latest) · [每夜版](https://github.com/Q2297045667/PumpGsit/releases?q=nightly) · [从源码构建](doc/构建与验证.md#从源码构建)
2. 停服，将 `PumpGsit.wasm` 放入服务端的 `plugins/`。从 Bukkit 的 GSit 迁移时，先备份旧库，并检查数据目录、三项偏好默认值和权限，可参考[旧服迁移与回滚](doc/存储.md)。
3. 使用默认 SQLite 时，在服务端的 `pumpkin.toml` 中加入以下配置，允许 Pumpkin 加载组件并访问插件数据目录。

   ```toml
   [plugins.overrides.PumpGsit]
   allow_unsigned = true
   allowed_permissions = ["fs.read.data", "fs.write.data"]
   ```

   使用 MySQL 时，把 `network.tcp.connect` 加入 `allowed_permissions`。数据库使用域名时，也加入 `network.dns`。在插件 `config.toml` 的 `[database]` 段填写连接参数。[MySQL 配置与切换](doc/存储.md#mysql-配置与切换)

   ```toml
   [plugins.overrides.PumpGsit]
   allow_unsigned = true
   allowed_permissions = ["fs.read.data", "fs.write.data", "network.tcp.connect", "network.dns"]
   ```

4. 启动后在控制台确认 `Loaded PumpGsit` 及所装版本。进游戏站在空旷处的完整方块上，执行 `/sit`，切到第三人称可看到屈膝坐姿。按潜行键起身。配置和语言文件在 `plugins/data/PumpGsit/` 的 `config.toml` 和 `lang/`。默认 SQLite 文件是同目录下的 `data/data.db`。

玩家用 `/gsitprefs` 查看互动偏好，打开聊天后点击允许修改的条目。服主可在控制台输入 `gsitprefs <玩家名或 UUID>` 查看迁移前后的值。[偏好面板与权限](doc/使用与配置.md#互动偏好面板)

## 使用与维护

| 要做的事 | 文档 |
| --- | --- |
| 设置座位、权限或语言 | [命令、权限与 TOML](doc/使用与配置.md) |
| 迁移或回滚 Bukkit 与旧 Pumpkin 插件的数据 | [存储、备份和回滚](doc/存储.md) |
| 在聊天或侧边栏显示姿势和偏好 | [六个占位符与 PumpkinPIE](doc/占位符.md) |
| 构建、测试或查看部署限制 | [构建与验证](doc/构建与验证.md) |
| 查看游戏画面与姿势实现 | [Java 客户端实测](doc/客户端验证.md) · [姿势实现](doc/姿势.md) |

原版作者为 Gecolay。PumpGsit 源码采用 GPL-3.0-only，随附的 Pumpkin WIT 保留各自的许可证。[原版 GSit 3.8.0 源码](https://github.com/gecolay/GSit/tree/04b792026acf95b63f198691d85218a1023e2f92) · [插件许可证](LICENSE) · [WIT 与许可证](wit/)
