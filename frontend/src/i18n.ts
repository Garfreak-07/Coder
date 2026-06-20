import type { NodeType } from "./types";

export const zhCN = {
  app: {
    title: "工作流工作台",
    eyebrow: "Planner-led 本地工作流",
    defaultStatus: "就绪"
  },
  templates: {
    title: "从模板开始",
    defaultCodingName: "Planner-led 默认工作流",
    defaultCodingPurpose: "Planner 决策，Executor 执行，Tester 返回证据；只传结构化交接信息。",
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
    plannerOnlyHuman: "只有 Planner 可询问用户",
    optionalModel: "OpenAI/DeepSeek 或 mock 模式",
    projectKnowledge: "项目摘要，可扩展本地 md/txt 知识库",
    structuredHandoff: "RunContract、PlannerOrder、ExecutionResult 等结构化 artifact",
    mediumRisk: "中：Executor 有实现职责，但默认不暴露直接写入工具",
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
  preflight: {
    eyebrow: "运行前检查",
    title: "Preflight 确认",
    running: "正在预检...",
    pass: "通过",
    warning: "有警告",
    error: "有错误",
    nodes: (nodes: number, edges: number) => `${nodes} 节点 / ${edges} 连线`,
    reachable: (nodes: number) => `${nodes} 个可达节点`,
    agents: (count: number) => `${count} 个智能体`,
    tokenBudget: (budget: string) => `Token 预算：${budget}`,
    stepBudget: (steps: number) => `最大步骤：${steps}`,
    toolBudget: (calls: number) => `最大工具调用：${calls}`,
    permissions: "权限摘要",
    editAgents: (count: number) => `${count} 个智能体可编辑文件`,
    commandAgents: (count: number) => `${count} 个智能体可运行命令`,
    networkAgents: (count: number) => `${count} 个智能体可使用网络`,
    approvalAgents: (count: number) => `${count} 个智能体需要审批`,
    approvalTools: (count: number) => `${count} 个工具需要审批`,
    scopes: "作用域",
    noScopes: "未限制作用域，将使用运行时默认路径保护。",
    tools: "工具风险",
    noTools: "此工作流没有工具节点。",
    issues: "问题",
    noIssues: "没有 error 或 warning。",
    errorsBlock: (count: number) => `${count} 个 error 阻止运行，请修复后重新预检。`,
    warningsConfirm: (count: number) => `${count} 个 warning，需要确认后才会启动。`,
    cancel: "取消",
    close: "关闭",
    confirm: "确认启动"
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
    loopMode: "循环模式",
    maxIterations: "最大迭代次数",
    itemsKey: "列表输入键",
    itemKey: "当前条目键",
    iterationKey: "迭代次数键",
    collectKey: "收集输出键",
    summaryKey: "循环摘要键",
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
  loop: "循环",
  human_gate: "人工审批",
  end: "结束"
};

export const nodeTypeDescriptions: Record<NodeType, string> = {
  start: "工作流入口",
  agent: "调用配置好的智能体",
  tool: "调用内置本地工具",
  mcp_tool: "调用 MCP stdio 工具",
  condition: "按状态表达式分支",
  loop: "显式循环控制节点，输出 continue、iteration 和 break_reason 供连线条件使用",
  human_gate: "等待用户批准或拒绝",
  end: "工作流结束"
};
