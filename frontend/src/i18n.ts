export const zh = {
  app: {
    eyebrow: "Coder v0.2",
    title: "本地优先工作流工作台"
  },
  actions: {
    applyJson: "应用 JSON",
    approve: "批准并继续",
    export: "导出",
    import: "导入",
    loadTemplate: "使用此模板",
    newBlank: "新建空白工作流",
    refresh: "刷新",
    refreshRuntime: "刷新运行状态",
    reject: "拒绝",
    rollback: "回滚快照",
    save: "保存",
    saveAgent: "保存 Agent",
    startRun: "启动实时运行"
  },
  labels: {
    agent: "Agent",
    agents: "Agent 配置",
    approvalAudit: "审批审计",
    approvalReason: "审批原因",
    checkResult: "检查结果",
    condition: "条件",
    contextPolicy: "上下文策略",
    edges: "连接",
    eventLog: "运行事件",
    goal: "目标",
    id: "ID",
    inspector: "检查器",
    instructions: "指令",
    libraryAgents: "库中的 Agent",
    liveRunDetail: "实时运行详情",
    maxTraversals: "最大穿越次数",
    model: "模型",
    name: "名称",
    nodeType: "节点类型",
    outputKey: "输出键",
    patchApply: "补丁应用",
    patchPreview: "补丁预览",
    pendingApproval: "待审批",
    permissions: "权限",
    priority: "优先级",
    provider: "供应商",
    repo: "项目目录",
    request: "任务请求",
    runtime: "运行状态",
    scopes: "作用域",
    storedRunDetail: "历史运行详情",
    storedRunHistory: "历史运行",
    tools: "工具",
    workflowAdvanced: "高级：工作流 JSON",
    workflowCanvas: "工作流画布",
    workflowLibrary: "工作流库"
  },
  helper: {
    advancedJson:
      "普通使用优先从模板开始；JSON 编辑保留给需要直接控制内部 schema 的高级用户。",
    noAgents: "当前工作流还没有 Agent。",
    noEvents: "暂无事件。",
    noFileChanges: "没有提议的文件变更。",
    noLiveRuns: "没有实时运行。",
    noSavedWorkflows: "还没有保存的工作流。",
    noSelection: "请选择一个节点或连接。",
    optionalApprovalReason: "可选：审批或拒绝原因",
    scopesPlaceholder: "可选：每行一个仓库相对路径",
    selectAgent: "选择 Agent"
  },
  permissions: {
    editFiles: "编辑文件",
    readFiles: "读取文件",
    requiresApproval: "需要审批",
    runCommands: "运行命令",
    useNetwork: "使用网络"
  },
  template: {
    name: "默认编码工作流",
    purpose: "Planner → Executor → Tester / Reviewer，覆盖项目索引、计划审批、补丁预览、应用审批、检查和复审。",
    agents: "Planner、Executor、Tester / Reviewer",
    tools: "project_index、recommend_modules、propose_patch、apply_patch、run_check、rollback_patch",
    approvals: "实施前审批、应用补丁前审批、命令审批",
    model: "支持 OpenAI / DeepSeek / OpenAI 兼容端点；无密钥时使用本地 mock。",
    knowledge: "当前阶段使用项目索引；本地文档知识库是后续 v0.2 项。",
    risk: "中等：默认需要人工审批后才会执行可变更步骤。"
  }
} as const;

export function nodeTypeLabel(type: string): string {
  const labels: Record<string, string> = {
    start: "开始",
    agent: "Agent",
    tool: "工具",
    mcp_tool: "MCP 工具",
    condition: "条件",
    human_gate: "人工审批",
    end: "结束"
  };
  return labels[type] ?? type;
}
