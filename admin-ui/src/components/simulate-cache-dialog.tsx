import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Loader2 } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { useSimulateCache, useSetSimulateCache } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'

interface SimulateCacheDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

// 格式化为百分比展示（0.8 -> 80%）
function toPercent(ratio: number): string {
  return `${Math.round(ratio * 1000) / 10}%`
}

export function SimulateCacheDialog({ open, onOpenChange }: SimulateCacheDialogProps) {
  const [enabled, setEnabled] = useState(false)
  const [readRatio, setReadRatio] = useState('0.8')
  const [writeRatio, setWriteRatio] = useState('0.1')

  const { data, isLoading } = useSimulateCache()
  const { mutate, isPending } = useSetSimulateCache()

  // 打开时用当前值预填
  useEffect(() => {
    if (open && data) {
      setEnabled(data.enabled)
      setReadRatio(String(data.readRatio))
      setWriteRatio(String(data.writeRatio))
    }
  }, [open, data])

  const read = Number.parseFloat(readRatio)
  const write = Number.parseFloat(writeRatio)
  const readValid = Number.isFinite(read) && read >= 0 && read <= 1
  const writeValid = Number.isFinite(write) && write >= 0 && write <= 1
  const sumValid = readValid && writeValid && read + write <= 1
  const inputShare = sumValid ? 1 - read - write : null

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (!readValid || !writeValid) {
      toast.error('读取/写入占比必须是 0~1 之间的数字')
      return
    }
    if (!sumValid) {
      toast.error('读取与写入占比之和不能超过 1')
      return
    }
    mutate(
      { enabled, readRatio: read, writeRatio: write },
      {
        onSuccess: (res) => {
          toast.success(
            `模拟缓存已${res.enabled ? '开启' : '关闭'}：读 ${toPercent(res.readRatio)} / 写 ${toPercent(res.writeRatio)}，即时生效`
          )
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
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>模拟缓存设置</DialogTitle>
        </DialogHeader>

        {isLoading ? (
          <div className="py-10 flex items-center justify-center text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin mr-2" /> 加载中...
          </div>
        ) : (
          <form onSubmit={handleSubmit}>
            <div className="space-y-4 py-4">
              {/* 开关 */}
              <div className="flex items-center justify-between">
                <div className="space-y-0.5">
                  <label htmlFor="sim-cache-enabled" className="text-sm font-medium">
                    启用模拟缓存
                  </label>
                  <p className="text-xs text-muted-foreground">
                    在响应 usage 中伪造 cache_read / cache_creation token（仅面板展示用）
                  </p>
                </div>
                <Switch
                  id="sim-cache-enabled"
                  checked={enabled}
                  onCheckedChange={setEnabled}
                  disabled={isPending}
                />
              </div>

              {/* 读取占比 */}
              <div className="space-y-2">
                <label htmlFor="sim-cache-read" className="text-sm font-medium">
                  缓存读取占比（0~1）
                </label>
                <Input
                  id="sim-cache-read"
                  type="number"
                  min="0"
                  max="1"
                  step="0.05"
                  value={readRatio}
                  onChange={(e) => setReadRatio(e.target.value)}
                  disabled={isPending}
                />
                <p className="text-xs text-muted-foreground">
                  每次请求把总输入 token 的该比例计入 cache_read（约 1/10 价计费）
                </p>
              </div>

              {/* 写入占比 */}
              <div className="space-y-2">
                <label htmlFor="sim-cache-write" className="text-sm font-medium">
                  缓存写入占比（0~1）
                </label>
                <Input
                  id="sim-cache-write"
                  type="number"
                  min="0"
                  max="1"
                  step="0.05"
                  value={writeRatio}
                  onChange={(e) => setWriteRatio(e.target.value)}
                  disabled={isPending}
                />
                <p className="text-xs text-muted-foreground">
                  计入 cache_creation（约 1.25 倍价计费），剩余部分为正常 input
                </p>
              </div>

              {/* 实时预览 */}
              <div className="rounded-md border bg-muted/50 px-3 py-2 text-sm">
                {sumValid ? (
                  <span>
                    拆分预览：读 <b>{toPercent(read)}</b> / 写 <b>{toPercent(write)}</b> / 正常 input{' '}
                    <b>{toPercent(inputShare!)}</b>
                  </span>
                ) : (
                  <span className="text-red-500">
                    占比无效：两项都需在 0~1 之间，且之和 ≤ 1
                  </span>
                )}
              </div>

              <p className="text-xs text-muted-foreground">
                保存后对后续请求即时生效（无需重启），并写回配置文件（重启后保持）。
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
              <Button type="submit" disabled={isPending || !sumValid}>
                {isPending ? '保存中...' : '保存'}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  )
}
