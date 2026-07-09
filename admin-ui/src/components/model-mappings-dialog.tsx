import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Loader2, Plus, Trash2, ChevronUp, ChevronDown } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useModelMappings, useSetModelMappings } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { ModelMapping } from '@/types/api'

interface ModelMappingsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

// 编辑态的行：keywords 用逗号分隔字符串，数值用字符串（便于输入）
interface MappingRow {
  id: string
  displayName: string
  keywords: string
  target: string
  contextWindow: string
  maxTokens: string
  created: number
}

function toRow(m: ModelMapping): MappingRow {
  return {
    id: m.id,
    displayName: m.displayName,
    keywords: m.keywords.join(', '),
    target: m.target,
    contextWindow: String(m.contextWindow),
    maxTokens: String(m.maxTokens),
    created: m.created,
  }
}

function emptyRow(): MappingRow {
  return {
    id: '',
    displayName: '',
    keywords: '',
    target: '',
    contextWindow: '200000',
    maxTokens: '64000',
    created: 0,
  }
}

// 行 → 映射规则；非法时返回错误消息
function parseRow(row: MappingRow, index: number): ModelMapping | string {
  const no = index + 1
  const id = row.id.trim()
  const displayName = row.displayName.trim()
  const target = row.target.trim()
  const keywords = row.keywords
    .split(/[,，]/)
    .map((k) => k.trim())
    .filter((k) => k.length > 0)
  const contextWindow = Number.parseInt(row.contextWindow, 10)
  const maxTokens = Number.parseInt(row.maxTokens, 10)

  if (!id) return `第 ${no} 条规则的模型 ID 不能为空`
  if (!displayName) return `第 ${no} 条规则的展示名称不能为空`
  if (keywords.length === 0) return `第 ${no} 条规则的匹配关键字不能为空`
  if (!target) return `第 ${no} 条规则的 Kiro 模型 ID 不能为空`
  if (!Number.isFinite(contextWindow) || contextWindow <= 0)
    return `第 ${no} 条规则的上下文窗口必须是大于 0 的整数`
  if (!Number.isFinite(maxTokens) || maxTokens <= 0)
    return `第 ${no} 条规则的最大输出必须是大于 0 的整数`

  return { id, displayName, keywords, target, contextWindow, maxTokens, created: row.created }
}

export function ModelMappingsDialog({ open, onOpenChange }: ModelMappingsDialogProps) {
  const [rows, setRows] = useState<MappingRow[]>([])

  const { data, isLoading } = useModelMappings()
  const { mutate, isPending } = useSetModelMappings()

  // 打开时用当前映射表预填
  useEffect(() => {
    if (open && data) {
      setRows(data.mappings.map(toRow))
    }
  }, [open, data])

  const updateRow = (index: number, patch: Partial<MappingRow>) => {
    setRows((prev) => prev.map((row, i) => (i === index ? { ...row, ...patch } : row)))
  }

  const removeRow = (index: number) => {
    setRows((prev) => prev.filter((_, i) => i !== index))
  }

  const moveRow = (index: number, delta: -1 | 1) => {
    setRows((prev) => {
      const next = [...prev]
      const target = index + delta
      if (target < 0 || target >= next.length) return prev
      ;[next[index], next[target]] = [next[target], next[index]]
      return next
    })
  }

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (rows.length === 0) {
      toast.error('至少需要一条映射规则')
      return
    }

    const mappings: ModelMapping[] = []
    const seen = new Set<string>()
    for (let i = 0; i < rows.length; i++) {
      const parsed = parseRow(rows[i], i)
      if (typeof parsed === 'string') {
        toast.error(parsed)
        return
      }
      if (seen.has(parsed.id)) {
        toast.error(`模型 ID 重复: ${parsed.id}`)
        return
      }
      seen.add(parsed.id)
      mappings.push(parsed)
    }

    mutate(
      { mappings },
      {
        onSuccess: (res) => {
          toast.success(`模型映射已更新（${res.mappings.length} 条规则），即时生效`)
          onOpenChange(false)
        },
        onError: (error: unknown) => {
          toast.error(`保存失败: ${extractErrorMessage(error)}`)
        },
      }
    )
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-4xl max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>模型映射设置</DialogTitle>
        </DialogHeader>

        {isLoading ? (
          <div className="py-10 flex items-center justify-center text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin mr-2" /> 加载中...
          </div>
        ) : (
          <form onSubmit={handleSubmit}>
            <div className="space-y-3 py-2">
              <p className="text-xs text-muted-foreground">
                请求模型名（小写、"." 归一为 "-" 后）包含某条规则的全部关键字即映射到对应的
                Kiro 模型；自上而下取第一条命中，顺序同时决定 /v1/models 的展示顺序。
                未命中任何规则的模型将返回 400「模型不支持」。
              </p>

              {rows.map((row, index) => (
                <div key={index} className="rounded-md border p-3 space-y-2">
                  <div className="flex items-center justify-between">
                    <span className="text-xs font-medium text-muted-foreground">
                      规则 {index + 1}
                    </span>
                    <div className="flex items-center gap-1">
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7"
                        onClick={() => moveRow(index, -1)}
                        disabled={isPending || index === 0}
                        title="上移（提高匹配优先级）"
                      >
                        <ChevronUp className="h-4 w-4" />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7"
                        onClick={() => moveRow(index, 1)}
                        disabled={isPending || index === rows.length - 1}
                        title="下移（降低匹配优先级）"
                      >
                        <ChevronDown className="h-4 w-4" />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7 text-red-500 hover:text-red-600"
                        onClick={() => removeRow(index)}
                        disabled={isPending}
                        title="删除此规则"
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  </div>

                  <div className="grid grid-cols-1 md:grid-cols-2 gap-2">
                    <div className="space-y-1">
                      <label className="text-xs text-muted-foreground">
                        模型 ID（/v1/models 展示，如 claude-sonnet-4-6）
                      </label>
                      <Input
                        value={row.id}
                        onChange={(e) => updateRow(index, { id: e.target.value })}
                        placeholder="claude-sonnet-4-6"
                        disabled={isPending}
                      />
                    </div>
                    <div className="space-y-1">
                      <label className="text-xs text-muted-foreground">展示名称</label>
                      <Input
                        value={row.displayName}
                        onChange={(e) => updateRow(index, { displayName: e.target.value })}
                        placeholder="Claude Sonnet 4.6"
                        disabled={isPending}
                      />
                    </div>
                    <div className="space-y-1">
                      <label className="text-xs text-muted-foreground">
                        匹配关键字（逗号分隔，须全部命中）
                      </label>
                      <Input
                        value={row.keywords}
                        onChange={(e) => updateRow(index, { keywords: e.target.value })}
                        placeholder="sonnet, 4-6"
                        disabled={isPending}
                      />
                    </div>
                    <div className="space-y-1">
                      <label className="text-xs text-muted-foreground">
                        Kiro 模型 ID（上游实际使用，如 claude-sonnet-4.6）
                      </label>
                      <Input
                        value={row.target}
                        onChange={(e) => updateRow(index, { target: e.target.value })}
                        placeholder="claude-sonnet-4.6"
                        disabled={isPending}
                      />
                    </div>
                    <div className="space-y-1">
                      <label className="text-xs text-muted-foreground">上下文窗口（tokens）</label>
                      <Input
                        type="number"
                        min="1"
                        step="1"
                        value={row.contextWindow}
                        onChange={(e) => updateRow(index, { contextWindow: e.target.value })}
                        disabled={isPending}
                      />
                    </div>
                    <div className="space-y-1">
                      <label className="text-xs text-muted-foreground">最大输出（tokens）</label>
                      <Input
                        type="number"
                        min="1"
                        step="1"
                        value={row.maxTokens}
                        onChange={(e) => updateRow(index, { maxTokens: e.target.value })}
                        disabled={isPending}
                      />
                    </div>
                  </div>
                </div>
              ))}

              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => setRows((prev) => [...prev, emptyRow()])}
                disabled={isPending}
              >
                <Plus className="h-4 w-4 mr-1" /> 添加规则
              </Button>

              <p className="text-xs text-muted-foreground">
                保存为整表替换：对后续请求即时生效（无需重启），并写回配置文件（重启后保持）。
              </p>
            </div>

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={isPending}
              >
                取消
              </Button>
              <Button type="submit" disabled={isPending}>
                {isPending ? '保存中...' : '保存'}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  )
}
