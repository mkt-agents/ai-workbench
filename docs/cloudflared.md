# Cloudflare Tunnel 使用说明

本应用在 **系统工具 → Cloudflare** 中管理本机 [cloudflared](https://developers.cloudflare.com/cloudflare-one/connections/connect-apps/) 隧道。实现代码：`src/components/CloudflaredManager.tsx`、`src-tauri/src/cloudflared_commands.rs`。

## 安装 cloudflared

任选其一：

1. **应用内 winget 安装**（需本机可用 winget）
2. **选择程序…**：指定已下载的 `cloudflared.exe`（无需配置 PATH）
3. **改用 PATH**：清除自定义路径，改从系统 PATH 查找
4. **打开下载页**：跳转 Cloudflare 官方下载说明

状态栏会显示是否已安装、版本与当前使用的路径。

## 临时隧道 vs 命名隧道

| 类型 | 用途 | 账号 | 公网地址 |
|------|------|------|----------|
| **临时** | 快速把本地 HTTP 服务暴露出去 | 不需要 | `*.trycloudflare.com`（进程结束即失效） |
| **命名** | 固定二级域名绑定 | 需要 Cloudflare 隧道 / Token 或本机 config | 你在控制台绑定的 hostname |

两者可以**同时运行多条**（不同本地端口 / 不同绑定）。同一命名绑定或同一本地 URL 的临时隧道不可重复启动。

### 临时隧道

1. 填写本地地址（如 `http://localhost:3000`）或点最近端口
2. 点「启动隧道」，等待日志中出现 trycloudflare URL
3. 右侧日志可按隧道过滤；运行中总览可复制/停止

### 命名隧道（二级域名绑定）

先在 Cloudflare Zero Trust（或 Dashboard）创建 Named Tunnel，并配置 **Public Hostname**（域名 → 你的本地服务）。本应用只负责在本机拉起 `cloudflared`。

认证二选一：

- **config.yml**：选择本机配置文件（常见于 `%USERPROFILE%\.cloudflared\config.yml`），需已含 credentials / ingress
- **Tunnel Token**：从控制台复制 token；本机仅保存，不上传

保存绑定后点「启动绑定」。启动命令示意：

```bash
# config 模式
cloudflared tunnel --no-autoupdate --config "<path>" run

# token 模式
cloudflared tunnel --no-autoupdate run --token "<token>"
```

## 多隧道与日志

- 后端以 `HashMap` 管理会话；停止时传会话 `id`，或「停止全部」
- 日志事件为结构化 `{ id, tag, line }`；UI 支持「全部 / 某一隧道」过滤
- 左侧可滚动；命名列表过长时在卡片内滚动，避免挡住临时隧道表单

## 数据落点

| 数据 | 位置 |
|------|------|
| 命名绑定列表 | SQLite 表 `cloudflared_profiles`（`ai-workbench.db`） |
| 自定义 exe 路径 | `%APPDATA%\com.ai-workbench.app\` 下应用写入的路径文件 |
| Tunnel Token / config 路径 | 随 profile 存本机 DB（明文） |

## 与 Zero Trust 的关系

- **DNS / Public Hostname / 访问策略** 在 Cloudflare 控制台完成
- 本应用不代替控制台创建隧道或改 DNS
- 若 config.yml 已写好 ingress，仍建议在绑定里填写 hostname，便于 UI 展示与复制公网 URL

## 常见问题

- **未安装**：先 winget 或选 exe
- **config.yml 不存在**：检查路径；浏览对话框默认尝试打开 `.cloudflared` 目录
- **已有隧道在跑（同 id）**：先停掉该条再启，或改用另一本地端口 / 另一绑定
- **临时 URL 迟迟不出**：看右侧日志；确认本地服务已监听对应端口
