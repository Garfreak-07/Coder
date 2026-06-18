# Coder

一个小巧、保守、可扩展的 LangGraph 代码协同工作流。

目标不是做一个“随便改代码的超级 Agent”，而是做一个任何人都能安全使用的闭环工具：

```text
读取需求 → 扫描项目 → 生成计划 → 人工确认 → 执行 → 检查 → 复盘/重试
```

当前版本仍然是安全骨架：默认 dry-run，不会直接修改代码。

## 设计原则

- 小而稳：先把核心闭环打牢，不急着堆功能。
- 人在回路：关键动作需要明确批准。
- 范围受限：只允许读取/修改用户指定的项目和模块。
- 可审计：计划、检查结果、风险判断都进入 state。
- 多模型：优先支持 OpenAI-compatible API，减少依赖数量。
- 默认安全：没有模型 key 时不会假装理解项目，只输出保守占位计划。

## 安装

```powershell
cd F:\bbb\coder
python -m venv .venv
.\.venv\Scripts\activate
pip install -e .
```

如果想把 pip 缓存也放到 F 盘：

```powershell
$env:PIP_CACHE_DIR="F:\bbb\pip-cache"
pip install -e .
```

## 配置模型

复制配置模板：

```powershell
Copy-Item .env.example .env
```

### OpenAI

```env
CODER_PROVIDER=openai
CODER_MODEL=gpt-4.1-mini
OPENAI_API_KEY=你的 key
```

### DeepSeek

```env
CODER_PROVIDER=deepseek
CODER_MODEL=deepseek-chat
DEEPSEEK_API_KEY=你的 key
```

### Kimi / Moonshot

```env
CODER_PROVIDER=kimi
CODER_MODEL=moonshot-v1-8k
MOONSHOT_API_KEY=你的 key
```

### Qwen / DashScope

```env
CODER_PROVIDER=qwen
CODER_MODEL=qwen-plus
DASHSCOPE_API_KEY=你的 key
```

### Ollama 本地模型

```env
CODER_PROVIDER=ollama
CODER_MODEL=qwen2.5-coder:7b
OLLAMA_BASE_URL=http://localhost:11434/v1
```

### 任意 OpenAI-compatible API

```env
CODER_PROVIDER=openai-compatible
CODER_MODEL=你的模型名
CODER_BASE_URL=https://你的服务地址/v1
CODER_API_KEY=你的 key
```

## 使用

分析当前项目：

```powershell
langgraph-coder --repo .
```

分析其他本地项目：

```powershell
langgraph-coder --repo "D:\projects\some-app"
```

限制目标范围：

```powershell
langgraph-coder --repo "D:\projects\some-app" --scope src/features/chat
```

带参考项目：

```powershell
langgraph-coder --repo "D:\projects\my-app" --reference "D:\projects\reference-app"
```

带检查命令：

```powershell
langgraph-coder --repo . --check "npm run typecheck"
```

临时覆盖模型配置：

```powershell
langgraph-coder --repo . --provider deepseek --model deepseek-chat
```

## 当前工作流

```text
START
  ↓
intake
  ↓
scan_repo
  ↓
plan
  ↓
approval
  ↓
execute
  ↓
check
  ↓
review
  ↓
done / retry / blocked
```

## 安全说明

当前 `execute` 节点是 dry-run：它记录“将要做什么”，但不直接写文件。

后续真正加入写代码能力时，应该保持这个顺序：

```text
生成 patch → 展示 diff → 人工确认 → 应用 patch → 检查 → 可回滚
```

不要把 API key 提交到仓库。`.env` 已被 `.gitignore` 忽略，`.env.example` 可以提交。
