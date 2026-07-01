import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { useQuery } from '@tanstack/react-query'
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
import { getCredentialDetail } from '@/api/credentials'
import { useUpdateCredential } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'

interface EditCredentialDialogProps {
  id: number
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function EditCredentialDialog({ id, open, onOpenChange }: EditCredentialDialogProps) {
  const [email, setEmail] = useState('')
  const [endpoint, setEndpoint] = useState('')
  const [region, setRegion] = useState('')
  const [authRegion, setAuthRegion] = useState('')
  const [apiRegion, setApiRegion] = useState('')
  const [proxyUrl, setProxyUrl] = useState('')
  const [proxyUsername, setProxyUsername] = useState('')
  const [proxyPassword, setProxyPassword] = useState('')
  // 中转渠道：'follow' 跟随全局 / 'on' 走中转 / 'off' 直连
  const [useRelay, setUseRelay] = useState<'follow' | 'on' | 'off'>('follow')

  // 打开时拉取当前值用于预填（含代理机密回显）
  const { data, isLoading } = useQuery({
    queryKey: ['credential-detail', id],
    queryFn: () => getCredentialDetail(id),
    enabled: open,
    staleTime: 0,
    gcTime: 0,
  })

  useEffect(() => {
    if (data) {
      setEmail(data.email ?? '')
      setEndpoint(data.endpoint ?? '')
      setRegion(data.region ?? '')
      setAuthRegion(data.authRegion ?? '')
      setApiRegion(data.apiRegion ?? '')
      setProxyUrl(data.proxyUrl ?? '')
      setProxyUsername(data.proxyUsername ?? '')
      setProxyPassword(data.proxyPassword ?? '')
      setUseRelay(data.useRelay == null ? 'follow' : data.useRelay ? 'on' : 'off')
    }
  }, [data])

  const { mutate, isPending } = useUpdateCredential()

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    mutate(
      {
        id,
        // 全量提交：空字符串在后端表示清除，非空表示设置
        req: {
          email,
          endpoint,
          region,
          authRegion,
          apiRegion,
          proxyUrl,
          proxyUsername,
          proxyPassword,
          useRelay,
        },
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          onOpenChange(false)
        },
        onError: (error: unknown) => {
          toast.error(`更新失败: ${extractErrorMessage(error)}`)
        },
      }
    )
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg max-h-[85vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>
            编辑凭据 #{id}
            {data?.authMethod ? ` · ${data.authMethod}` : ''}
          </DialogTitle>
        </DialogHeader>

        {isLoading ? (
          <div className="py-10 flex items-center justify-center text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin mr-2" /> 加载中...
          </div>
        ) : (
          <form onSubmit={handleSubmit} className="flex flex-col min-h-0 flex-1">
            <div className="space-y-4 py-4 overflow-y-auto flex-1 pr-1">
              {/* Email */}
              <div className="space-y-2">
                <label htmlFor="edit-email" className="text-sm font-medium">
                  Email（显示名）
                </label>
                <Input
                  id="edit-email"
                  placeholder="留空清除"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  disabled={isPending}
                />
              </div>

              {/* 中转渠道（账号级） */}
              <div className="space-y-2">
                <label htmlFor="edit-useRelay" className="text-sm font-medium">
                  中转渠道
                </label>
                <select
                  id="edit-useRelay"
                  value={useRelay}
                  onChange={(e) => setUseRelay(e.target.value as 'follow' | 'on' | 'off')}
                  disabled={isPending}
                  className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <option value="follow">跟随全局</option>
                  <option value="on">走中转</option>
                  <option value="off">直连（不走中转）</option>
                </select>
                <p className="text-xs text-muted-foreground">
                  仅此账号生效，保存即时生效无需重启。「走中转」需全局已配置中转 url + apiKey
                </p>
              </div>

              {/* 端点 */}
              <div className="space-y-2">
                <label htmlFor="edit-endpoint" className="text-sm font-medium">
                  端点
                </label>
                <Input
                  id="edit-endpoint"
                  placeholder="留空使用默认端点（如 ide / cli）"
                  value={endpoint}
                  onChange={(e) => setEndpoint(e.target.value)}
                  disabled={isPending}
                />
                <p className="text-xs text-muted-foreground">
                  留空使用全局 defaultEndpoint；填写时须为已注册端点
                </p>
              </div>

              {/* Region 配置 */}
              <div className="space-y-2">
                <label className="text-sm font-medium">Region 配置</label>
                <div className="grid grid-cols-3 gap-2">
                  <Input
                    placeholder="Region"
                    value={region}
                    onChange={(e) => setRegion(e.target.value)}
                    disabled={isPending}
                  />
                  <Input
                    placeholder="Auth Region"
                    value={authRegion}
                    onChange={(e) => setAuthRegion(e.target.value)}
                    disabled={isPending}
                  />
                  <Input
                    placeholder="API Region"
                    value={apiRegion}
                    onChange={(e) => setApiRegion(e.target.value)}
                    disabled={isPending}
                  />
                </div>
                <p className="text-xs text-muted-foreground">
                  均可留空使用全局配置。Auth Region 用于 Token 刷新，API Region 用于 API 请求
                </p>
              </div>

              {/* 代理配置 */}
              <div className="space-y-2">
                <label className="text-sm font-medium">代理配置</label>
                <Input
                  id="edit-proxyUrl"
                  placeholder='代理 URL（留空使用全局配置，"direct" 不使用代理）'
                  value={proxyUrl}
                  onChange={(e) => setProxyUrl(e.target.value)}
                  disabled={isPending}
                />
                <div className="grid grid-cols-2 gap-2">
                  <Input
                    id="edit-proxyUsername"
                    placeholder="代理用户名"
                    value={proxyUsername}
                    onChange={(e) => setProxyUsername(e.target.value)}
                    disabled={isPending}
                  />
                  <Input
                    id="edit-proxyPassword"
                    type="password"
                    placeholder="代理密码"
                    value={proxyPassword}
                    onChange={(e) => setProxyPassword(e.target.value)}
                    disabled={isPending}
                  />
                </div>
                <p className="text-xs text-muted-foreground">
                  支持 http/https/socks5，或特殊值 "direct"。清空 URL 即移除该凭据的代理
                </p>
              </div>
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
