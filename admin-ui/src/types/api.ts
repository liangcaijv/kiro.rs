// 凭据状态响应
export interface CredentialsStatusResponse {
  total: number
  available: number
  currentId: number
  credentials: CredentialStatusItem[]
}

// 单个凭据状态
export interface CredentialStatusItem {
  id: number
  priority: number
  disabled: boolean
  failureCount: number
  isCurrent: boolean
  expiresAt: string | null
  authMethod: string | null
  hasProfileArn: boolean
  email?: string
  refreshTokenHash?: string
  apiKeyHash?: string
  maskedApiKey?: string
  successCount: number
  lastUsedAt: string | null
  hasProxy: boolean
  proxyUrl?: string
  refreshFailureCount: number
  disabledReason?: string
  endpoint: string
  // 账号级中转开关：undefined/null=跟随全局，true=走中转，false=直连
  useRelay?: boolean | null
}

// 余额响应
export interface BalanceResponse {
  id: number
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  nextResetAt: number | null
}

// 成功响应
export interface SuccessResponse {
  success: boolean
  message: string
}

// 错误响应
export interface AdminErrorResponse {
  error: {
    type: string
    message: string
  }
}

// 请求类型
export interface SetDisabledRequest {
  disabled: boolean
}

export interface SetPriorityRequest {
  priority: number
}

// 添加凭据请求
export interface AddCredentialRequest {
  refreshToken?: string
  authMethod?: 'social' | 'idc' | 'api_key' | 'external_idp'
  clientId?: string
  clientSecret?: string
  // 外部 IdP（external_idp / Microsoft Entra ID）刷新所需
  tokenEndpoint?: string
  issuerUrl?: string
  scopes?: string
  priority?: number
  authRegion?: string
  apiRegion?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  kiroApiKey?: string
  endpoint?: string
}

// 添加凭据响应
export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
}

// 凭据可编辑字段详情（编辑表单预填，含代理机密回显）
export interface CredentialDetail {
  id: number
  authMethod?: string
  email?: string
  endpoint?: string
  region?: string
  authRegion?: string
  apiRegion?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  // 账号级中转开关：undefined/null=跟随全局，true=走中转，false=直连
  useRelay?: boolean | null
}

// 编辑凭据请求（PATCH 语义：字段空字符串=清除，非空=设置）
export interface UpdateCredentialRequest {
  email?: string
  endpoint?: string
  region?: string
  authRegion?: string
  apiRegion?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  // 中转开关三态字符串：'follow' | 'on' | 'off'
  useRelay?: string
}

// 模拟缓存设置（enabled + 读/写占比，0~1）
export interface SimulateCacheConfig {
  enabled: boolean
  readRatio: number
  writeRatio: number
}

// 设置模拟缓存请求（省略的字段保持当前值不变）
export interface SetSimulateCacheRequest {
  enabled?: boolean
  readRatio?: number
  writeRatio?: number
}

// 单条模型映射规则：请求模型名（小写、"." 归一为 "-" 后）包含全部 keywords
// 即映射到 target（Kiro 上游模型 ID）；顺序即匹配优先级与 /v1/models 展示顺序
export interface ModelMapping {
  id: string
  displayName: string
  keywords: string[]
  target: string
  contextWindow: number
  maxTokens: number
  created: number
}

// 模型映射表（GET 响应与 PUT 请求同构，PUT 为整表替换）
export interface ModelMappingsConfig {
  mappings: ModelMapping[]
}
