import type { NodeType } from "./types";

export const zhCN = {
  app: {
    title: "工作流工作台",
    eyebrow: "Coder v2 本地优先",
    defaultStatus: "就绪"
  },
  templates: {
    title: "从模板开始",
    defaultCodingName: "默认编码工作流",
    defaultCodingPurpose: "Planner → Executor → Tester / Reviewer，用审批门控制实现和补丁应用。",
    blankName: "空白高级工作流",
    blankPurpose: "只包含 start/end，适合手动搭建或粘贴 JSON。",
    useTemplate: "使用模板",
    agents: "智能体",
    tools: "工具",
    approvals: "审批",
    model: "模型",
    knowledge: "知识源",
    risk: "风险",
    requiredApprovals: "实现前审批、应用补丁前审批",
    optionalModel: "OpenAI/DeepSeek 或 mock 模式",
    projectKnowledge: "项目摘要，可扩展本地 md/txt 知识库",
    mediumRisk: "中：可提议和应用受控补丁",
    lowRisk: "低：不包含可写节点"
  },
  library: {
    title: "工作流库",
    loadExample: "加载编码示例",
    refresh: "刷新",
    empty: "还没有保存的工作流。",
    nodeEdgeCount: (nodes: number, edges: number) => `${nodes} 个节点 / ${edges} 条边`
  },
  run: {
    title: "运行",
    repo: "项目路径",
    scopes: "作用域",
    scopesPlaceholder: "可选：仓库相对路径，每行一个",
    request: "需求",
    preApprove: "预先通过审批门",
    start: "启动实时运行"
  },
  runtime: {
    title: "运行时",
    refresh: "刷新运行状态",
    unknown: "未知",
    tools: (count: number) => `${count} 个工具`,
    liveRuns: (count: number) => `${count} 个实时运行`,
    storedRuns: (count: number) => `${count} 条历史运行`,
    noLiveRuns: "没有实时运行。",
    storedHistory: "历史运行",
    noStoredRuns: "没有历史运行。"
  },
  canvas: {
    addNode: (type: NodeType) => `+ ${nodeTypeLabels[type]}`
  },
  json: {
    title: "工作流 JSON（高级）",
    apply: "应用 JSON",
    save: "保存",
    export: "导出",
    import: "导入"
  },
  inspector: {
    title: "检查器",
    empty: "选择一个节点或连线。",
    agents: "智能体",
    addAgent: "+ 智能体",
    saveAgent: "保存智能体",
    noAgents: "当前工作流没有智能体。",
    libraryAgents: "库中的智能体"
  },
  events: {
    title: "运行事件",
    empty: "还没有事件。"
  },
  forms: {
    id: "ID",
    type: "类型",
    agent: "智能体",
    selectAgent: "选择智能体",
    tool: "工具",
    mcpToolName: "MCP 工具名",
    inputJson: "输入 JSON",
    condition: "条件",
    approvalReason: "审批原因",
    outputKey: "输出键",
    from: "从",
    to: "到",
    priority: "优先级",
    maxTraversals: "最大经过次数",
    name: "名称",
    role: "角色",
    goal: "目标",
    instructions: "指令",
    provider: "Provider",
    model: "Model",
    permissions: "权限",
    readFiles: "读取文件",
    editFiles: "编辑文件",
    runCommands: "运行命令",
    useNetwork: "使用网络",
    requiresApproval: "需要审批",
    contextPolicy: "上下文策略",
    inputKeys: "输入键",
    summaryKeys: "摘要键"
  }
};

export const nodeTypeLabels: Record<NodeType, string> = {
  start: "开始",
  agent: "智能体",
  tool: "工具",
  mcp_tool: "MCP 工具",
  condition: "条件",
  human_gate: "人工审批",
  end: "结束"
};

export const nodeTypeDescriptions: Record<NodeType, string> = {
  start: "工作流入口",
  agent: "调用配置好的智能体",
  tool: "调用内置本地工具",
  mcp_tool: "调用 MCP stdio 工具",
  condition: "按状态表达式分支",
  human_gate: "等待用户批准或拒绝",
  end: "工作流结束"
};
