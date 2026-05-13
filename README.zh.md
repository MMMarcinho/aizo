# aizo 爱憎

**aizo**（爱憎，*ài zēng*）是一个专为 AI 智能体设计的轻量、高性能偏好记忆系统，完全用 Rust 构建。

它模仿人类认知记忆的工作方式：不存储完整的对话记录，而是持续地从历史交互中**提取、量化、衰减、召回**用户稳定的偏好、厌恶、习惯、沟通风格和硬性边界。最终形成一个紧凑的、带数值权重的个性画像，任何智能体都能在毫秒内完成查询。

---

## 整体架构

aizo 运行于两个互补的循环中：

```
╔══════════════════════════════════════════════════════════════════════╗
║  循环 1 — 会话内（实时响应：即时捕捉用户情绪信号）                  ║
╚══════════════════════════════════════════════════════════════════════╝

   用户 ──► 智能体（如 Claude Code）──── aizo add ────────────┐
                                                               ▼
                         CLAUDE.md ◄── 反哺 ────────── 本地 SQLite
                                                      （用户偏好库）


╔══════════════════════════════════════════════════════════════════════╗
║  循环 2 — 后台任务（定时批量分析积累的会话记录）                     ║
╚══════════════════════════════════════════════════════════════════════╝

   用户 ──► openclaw 等 ──► 会话记录 ──── aizo analyze ────────┐
                                                                ▼
   USER.md、SOUL.md、IDENTITY.md … ◄── 反哺 ────────── 本地 SQLite
                                                       （用户偏好库）
```

**循环 1（SOP 1–6）：** 智能体在会话中实时检测偏好信号，立即调用 `aizo add` 写入。更新后的画像反哺进 `CLAUDE.md`，下次会话开始时即可加载最新认知。

**循环 2（SOP 7）：** 定时任务对积累的会话记录运行 `aizo analyze`，提取实时循环遗漏的隐式信号。结果写入更丰富的身份文件（`USER.md`、`SOUL.md`、`IDENTITY.md`），在所有智能体之间持久化用户画像。

---

## 核心设计

```
对话记录
    │
    ▼
Flash LLM（claude-haiku-4-5）
    │  语义提取
    ▼
结构化词条  { 标签, 基础分数 0–10 }
    │  平滑合并
    ▼
SQLite（~/.aizo/preferences.db）
    │
    ▼
有效权重 = s · d(t)^α   （分数调制的衰减）
    │  关键词或 Top-N 召回
    ▼
智能体读取画像 → 个性化响应
```

### 评分公式

所有评分逻辑集中在 `src/scoring/mod.rs`。每条偏好词条在读取时，根据 `base_score` 和 `last_seen` 时间戳实时计算三个字段。

**第一步 — 衰减系数** $d(t)$

$$d(t) = \phi + (1 - \phi) \cdot e^{-\lambda t}, \quad \lambda = \frac{\ln 2}{t_{1/2}}$$

$t$ 为距上次触发的天数，$t_{1/2}$ 为配置的半衰期，$\phi$ 为衰减下限。

**第二步 — 分数指数** $\alpha$

$$\alpha = \frac{10 - s}{10}$$

分数越高 → $\alpha$ 越小 → 衰减影响越弱。分数为 10 时（$\alpha = 0$），完全不受时间影响；分数为 0 时（$\alpha = 1$），以最快速度衰减。

**第三步 — 有效权重** $w$

$$w = s \cdot d(t)^{\alpha}$$

展开为单一表达式：

$$\boxed{w = s \cdot \left[\phi + (1-\phi) \cdot e^{-\lambda t}\right]^{\frac{10-s}{10}}}$$

**直觉解释：你越爱的东西，越不容易被遗忘；无所谓的习惯，慢慢就淡了。**

**边界行为**

| 分数 $s$ | $\alpha$ | 衰减效果 | 含义 |
|---|---|---|---|
| 10 | 0.0 | 无 — $d^0 = 1$ | 核心价值观，永不消退 |
| 7  | 0.3 | 轻微 | 强烈偏好，缓慢消退 |
| 5  | 0.5 | 中等 | 中性习惯，半速消退 |
| 1  | 0.9 | 接近完全 | 弱厌恶，快速消退 |
| 0  | 1.0 | 完全 | $w = 0$，始终为零 |

词条**永远不会因衰减而被删除**——它们会沉降至下限，以弱长期记忆的形式保留。

### 分数量表（0–10）

没有"类别"字段。`base_score` 是唯一的情感维度，也是 `--type` 过滤的依据：

| 分数 | 含义 | `--type` 别名 |
|---|---|---|
| 0–1.5 | 硬性边界 / 绝对禁忌 | `taboo` |
| 1.6–4 | 明确厌恶 | `aversion` |
| 4–6.5 | 中性倾向 / 弱习惯 | `habit` |
| 6.5–10 | 风格 / 沟通偏好 | `style` |
| 7–10 | 明确偏好 | `preference` |

在 `recall` 和 `top` 中使用 `--type` 按分数范围过滤，逗号分隔可多选：

```bash
aizo recall code --type preference,habit,style,taboo
aizo recall --type taboo               # 列出所有硬性边界
aizo top 5 --type preference
```

使用 `--keywords`（`add` 时）或 `aizo tag` 添加任意分类标签。

### 分数平滑

同一词条在多次会话中被提及时：

```
新基础分数 = 旧基础分数 × 0.4 + 新提取分数 × 0.6
```

`last_seen` 每次都会刷新，重置衰减时钟。

---

## 安装

```bash
# Cargo（推荐）
cargo install aizo

# npm / npx
npm install -g aizo
npx aizo top 10

# 从源码编译（需要 Rust ≥ 1.70）
git clone https://github.com/mmmarcinho/aizo
cd aizo && cargo build --release
cp target/release/aizo /usr/local/bin/aizo
```

### 初次配置

运行交互向导——自动写入 `~/.aizo/.env` 并测试连接：

```bash
aizo init
```

或手动设置环境变量：

```bash
# Anthropic
export ANTHROPIC_API_KEY=sk-ant-...

# 任意 OpenAI 兼容模型（Ollama、OpenRouter、DeepSeek、vLLM…）
export AIZO_MODEL=qwen2.5:7b
export AIZO_API_URL=http://localhost:11434/v1/chat/completions
```

### 配置环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `AIZO_DB_PATH` | `~/.aizo/preferences.db` | SQLite 数据库路径 |
| `ANTHROPIC_API_KEY` | — | Anthropic API 密钥（自动检测） |
| `AIZO_API_KEY` | — | 任意提供商的 API 密钥 |
| `AIZO_API_URL` | 提供商默认值 | LLM 端点 URL |
| `AIZO_MODEL` | `claude-haiku-4-5` | 模型名称 |
| `AIZO_API_FORMAT` | 自动 | `anthropic` 强制使用 Anthropic 协议 |
| `AIZO_MAX_TOKENS` | `8192` | LLM 最大输出 token 数 |
| `AIZO_AUTO_KEYWORDS` | `false` | `true` 启用 LLM 自动生成关键词 |

所有变量均可在 `~/.aizo/.env`（用户级）或 `./.env`（项目级）中设置。Shell 环境变量优先级最高。

---

## 命令参考

```
aizo [--db <路径>] <命令>
```

| 命令 | 说明 |
|---|---|
| `init` | 交互式配置向导——写入 `~/.aizo/.env`，测试连接 |
| `analyze [文件]` | 用 LLM 分析会话文件或 JSON/JSONL 导出 |
| `extract [文件]` | 将提取提示词输出到 stdout（可接管道给任意 LLM） |
| `import` | 从 stdin 读取 `{"entries":[…]}` JSON 并批量写入 |
| `recall [查询]` | 关键词+分数范围召回 — **智能体核心调用** |
| `top [N]` | 按有效权重排列的前 N 条（默认 10） |
| `show` | 输出完整画像，按有效权重排序 |
| `add <标签> <原因>` | 手动添加或更新一条偏好（原因须加引号） |
| `tag <标签> <关键词…>` | 为已有词条添加或替换关键词 |
| `touch <标签…>` | 重置衰减时钟，不修改分数 |
| `remove <标签…>` | 硬删除一条词条 |
| `keywords` | 列出所有已存储的关键词及词条数 |
| `clear` | 清空整个画像和会话历史 |
| `info` | 显示数据库路径、分数分布、衰减配置 |
| `config show/set-half-life/set-floor` | 查看或设置衰减参数 |

**`recall` 标志：**

| 标志 | 说明 |
|---|---|
| `--type/-t <类型>` | 分数范围过滤，逗号分隔：`preference`、`style`、`habit`、`aversion`、`taboo` |
| `--limit/-l <N>` | 按有效权重排序后限制返回数量 |
| `--scenario <名称>` | 扩展为预设关键词列表：`coding`、`writing`、`communication` |
| `--no-touch` | 不刷新匹配词条的 `last_seen` |
| `--json` | 输出原始 JSON（供程序调用） |

**`top` / `show` / `recall` 标志：** `--json` 输出原始 JSON。

**`top` 标志：** `--type/-t` 分数范围过滤，同 recall。

### 使用示例

```bash
# 分析对话记录（支持文本、JSON、JSONL）
aizo analyze ./chat.txt
aizo analyze ./export.json
cat conversation.md | aizo analyze

# 智能体生成前召回偏好
aizo top 5
aizo recall "代码风格"

# 场景化召回（coding 自动展开为约 10 个相关关键词）
aizo recall --scenario coding --type preference,style,habit,taboo --limit 20

# 仅按类型召回（无需关键词）
aizo recall --type taboo                        # 所有硬性边界
aizo recall code --type preference --limit 10   # 前 10 条编码偏好
aizo recall code --type preference,habit --limit 20  # 多类型

# 查看完整画像
aizo show
aizo show --json   # 原始 JSON，供程序调用

# 手动录入 — 分数即情感
aizo add "简洁的代码"        "总是要求更短的实现"          --score 9.0
aizo add "冗长注释"          "多次抱怨过度文档化的代码"     --score 1.5
aizo add "输出中使用表情符号" "明确说过永远不要用"           --score 0.5
aizo add "使用深色模式"      "每次 UI 会话都提到深色主题"   --score 5.0

# 关键词管理（用于更丰富的召回）
aizo tag "简洁的代码" brevity minimal short lean
aizo keywords

# 调整衰减参数（默认：半衰期 30 天，下限 0.1）
aizo config set-half-life 14
aizo config set-floor 0.05

# 查看统计
aizo info
```

---

## 词条格式

```json
{
  "id": 1,
  "item": "简洁的代码",
  "reason": "总是要求更短的实现，不喜欢多余内容。",
  "keywords": ["brevity", "minimal", "short", "lean"],
  "base_score": 9.0,
  "source": "analysis",
  "added_at": "2026-05-07T14:00:00+00:00",
  "last_seen": "2026-05-07T15:30:00+00:00",
  "score_exponent": 0.1,
  "decay_coefficient": 0.87,
  "effective_weight": 8.78
}
```

---

## 数据库结构

```sql
CREATE TABLE preferences (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    item        TEXT    NOT NULL,
    reason      TEXT    NOT NULL,
    keywords    TEXT    NOT NULL DEFAULT '',    -- 逗号分隔的同义词标签
    base_score  REAL    NOT NULL DEFAULT 5.0,   -- 0-10
    source      TEXT    NOT NULL DEFAULT 'manual',
    added_at    TEXT    NOT NULL,
    last_seen   TEXT    NOT NULL                -- 每次强化时重置衰减时钟
);
-- UNIQUE 约束：LOWER(item)

CREATE TABLE decay_config (
    id              INTEGER PRIMARY KEY CHECK(id = 1),
    half_life_days  REAL    NOT NULL DEFAULT 30.0,
    floor           REAL    NOT NULL DEFAULT 0.1
);

CREATE TABLE sessions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    analyzed_at  TEXT    NOT NULL,
    extracted    INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT    NOT NULL DEFAULT ''
);
```

---

## 智能体集成

任何智能体都可以将 aizo 作为子进程调用——无需向量索引，无需额外运行时：

```python
import subprocess, json

def top_preferences(n: int = 10) -> list[dict]:
    return json.loads(subprocess.check_output(["aizo", "top", str(n), "--json"]))

def recall(query: str, types: str = "preference,style,habit,taboo") -> list[dict]:
    return json.loads(subprocess.check_output(
        ["aizo", "recall", query, "--type", types, "--json"]
    ))

def recall_scenario(scenario: str) -> list[dict]:
    return json.loads(subprocess.check_output(
        ["aizo", "recall", "--scenario", scenario,
         "--type", "preference,style,habit,taboo", "--limit", "20", "--json"]
    ))

# 生成前注入系统提示
prefs = top_preferences(20)
system = f"用户偏好：\n{json.dumps(prefs, indent=2, ensure_ascii=False)}\n\n{base_system}"

# 写代码前检查编程偏好
coding_prefs = recall_scenario("coding")
```

通过环境变量为每个项目维护独立画像：

```bash
export AIZO_DB_PATH=./project-prefs.db
aizo show
```

---

## 标准操作规程（SOP）

智能体使用 aizo 的 SOP 定义在 `skills/aizo-sop.md`。将其复制到智能体的指令目录（如 Claude Code 的 `.claude/skills/`），该项目中的智能体即可自动遵循此规程。

该技能文件定义了七个触发时机：

| # | 触发时机 | aizo 调用 | 执行时序 |
|---|---|---|---|
| 1 | 会话开始 | `aizo top 20` → 格式化为文字摘要 | 同步，首次回复前 |
| 2 | 用户表达负面反馈 | `aizo add … --score 1.5` 再 `aizo recall <主题>` | 同步，修正回复前 |
| 3 | 用户表达称赞 | `aizo add … --score 9.0` | 异步，回复发送后 |
| 4 | 用户下达明确指令 | `aizo add … --score 0.5` 或 `--score 10` | 同步，立即执行 |
| 5 | 即将针对主题 X 生成内容 | `aizo recall --scenario <X>` 或 `aizo recall <X> --type preference,style,taboo` | 同步，生成前 |
| 6 | 会话结束 | `aizo analyze <对话记录>` | 异步，后台执行 |
| 7 | 每日定时任务 | 智能体 LLM 扫描日志 → `aizo touch` 确认词条 | 定时，后台执行 |

**技能文件中的关键规则：**
- 分数极低（≤ 1.5）的词条优先级永远最高
- `analyze` 用于完整会话，不用于单条消息——它会调用 LLM
- `recall` 返回空结果意味着无数据，而非中性偏好
- 永远不要向用户提及 aizo——它在后台静默运行

---

## 开发

```bash
cargo build
cargo build --release
cargo test
```

---

## 许可证

MIT
