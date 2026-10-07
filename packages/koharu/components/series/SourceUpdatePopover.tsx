'use client'

import { AlertCircle, Check, Link2, LoaderCircle, RefreshCw, Unlink } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import {
  useCancelFetch,
  useCheckSeriesUpdates,
  useSetSeriesSource,
  useStartFetch,
} from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { type SourceCheck } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'
import { Popover, PopoverContent, PopoverTrigger } from '@koharu/ui/components/popover'
import { Progress } from '@koharu/ui/components/progress'

/** Where a series' raw chapters come from, and the button that fetches the ones it does not have.
 *
 * A popover rather than a settings section: binding happens when a new chapter lands, not once per
 * series in a settings form. The address input lives here for the same reason — the moment someone
 * pastes a URL is the moment they are thinking about it.
 *
 * A Popover rather than a DropdownMenu because it takes text input, and Base UI's menu typeahead
 * would swallow every character of a pasted address. */
export function SourceUpdatePopover({
  series,
  source,
  kind,
  busy,
}: {
  series: string
  source: { site: string; slug: string } | null
  /** The series' existing form, shown so inheritance is visible rather than magic. */
  kind: 'manga' | 'webtoon' | null
  busy: boolean
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [address, setAddress] = useState('')
  const [check, setCheck] = useState<SourceCheck | null>(null)
  const [picked, setPicked] = useState<string[]>([])

  const { setSource, settingSource } = useSetSeriesSource(series)
  const { checkUpdates, checkingUpdates } = useCheckSeriesUpdates(series)
  const { startFetch, startingFetch } = useStartFetch(series)
  const { cancelFetch } = useCancelFetch()

  const fetch = useKoharuStore((state) => state.sourceFetches[series])
  const dismissFetch = useKoharuStore((state) => state.dismissSourceFetch)
  const running = fetch?.state === 'running'

  // A terminal state stays in the store after the run ends so a failure is still readable, which
  // means something has to clear it. Closing the popover is when the user is done looking — and it
  // has to be the close itself rather than an effect on `open`, because an effect also runs on mount
  // and would throw away a state the app may already be holding.
  const reopen = (next: boolean) => {
    if (!next) dismissFetch(series)
    setOpen(next)
  }

  const bind = async () => {
    if (!address.trim()) return
    await setSource(address.trim())
    setAddress('')
  }

  const runCheck = async () => {
    // 一个旧的终态不该和这一次的结果并排显示：成功行会立刻变成误导，而失败行是上一轮的。
    dismissFetch(series)
    const result = await checkUpdates()
    setCheck(result)
    // Everything missing is picked by default: the list is there so the user can drop a chapter
    // they do not want, not so they have to opt into each one.
    setPicked(result.missing.map((chapter) => chapter.name))
  }

  const chosen = (check?.missing ?? []).filter((chapter) => picked.includes(chapter.name))

  const download = async () => {
    await startFetch(chosen)
    setCheck(null)
    setPicked([])
  }

  return (
    <Popover open={open} onOpenChange={reopen}>
      <PopoverTrigger
        render={
          <Button
            type='button'
            size='sm'
            variant='outline'
            disabled={busy}
            aria-busy={running || startingFetch}
            className='h-7 gap-1.5 text-[10px]'
          />
        }
      >
        {running ? (
          <LoaderCircle className='size-3 animate-spin' />
        ) : (
          <RefreshCw className='size-3' />
        )}
        {t('series.source.fetch')}
      </PopoverTrigger>
      <PopoverContent align='end' className='w-96 gap-2 p-2'>
        {source ? (
          <>
            <header className='grid gap-0.5 px-0.5'>
              <p className='truncate text-[10px] font-medium'>
                <span className='text-muted-foreground'>{t('series.source.site')}</span>
                {' · '}
                {source.slug}
              </p>
              <p className='text-[9px] text-muted-foreground'>
                {t(`series.source.${check?.kind ?? kind ?? 'manga'}`)}
              </p>
            </header>

            {!running && (
              <Button
                type='button'
                size='sm'
                variant='outline'
                disabled={checkingUpdates}
                aria-busy={checkingUpdates}
                className='h-7 gap-1.5 text-[10px]'
                onClick={() => void runCheck().catch(() => undefined)}
              >
                {checkingUpdates ? (
                  <LoaderCircle className='size-3 animate-spin' />
                ) : (
                  <RefreshCw className='size-3' />
                )}
                {t('series.source.check')}
              </Button>
            )}

            {running && fetch && (
              <div className='grid gap-1 px-0.5'>
                <Progress value={percent(fetch.chapter_index, fetch.chapter_total)} />
                <p className='text-[9px] text-muted-foreground'>
                  <span className='tabular-nums'>
                    {fetch.chapter_index}/{fetch.chapter_total}
                  </span>
                  {fetch.chapter ? ` · ${fetch.chapter}` : ''}
                  {fetch.page_total > 0 && (
                    <span className='tabular-nums'>
                      {' · '}
                      {t('series.source.pages', {
                        done: fetch.page_index,
                        total: fetch.page_total,
                      })}
                    </span>
                  )}
                </p>
                <Button
                  type='button'
                  size='sm'
                  variant='ghost'
                  className='h-6 text-[10px]'
                  onClick={() => void cancelFetch(fetch.id).catch(() => undefined)}
                >
                  {t('common.cancel')}
                </Button>
              </div>
            )}

            {/* 三种终态要分开：把成功说成失败，用户会以为白下载了一遍而再去下一次。 */}
            {fetch && !running && (
              <p
                className={
                  fetch.state === 'failed'
                    ? 'flex items-start gap-1 px-0.5 text-[9px] text-destructive'
                    : 'flex items-start gap-1 px-0.5 text-[9px] text-muted-foreground'
                }
              >
                {fetch.state === 'failed' ? (
                  <AlertCircle className='mt-px size-3 shrink-0' />
                ) : (
                  <Check className='mt-px size-3 shrink-0' />
                )}
                <span className='min-w-0 break-words'>
                  {fetch.state === 'finished'
                    ? t('series.source.done', { count: fetch.chapter_total })
                    : fetch.state === 'stopped'
                      ? t('series.source.stopped')
                      : (fetch.error ?? t('series.source.failed'))}
                </span>
              </p>
            )}

            {check && !running && (
              <>
                {check.missing.length === 0 ? (
                  <p className='flex items-center gap-1 px-0.5 text-[9px] text-muted-foreground'>
                    <Check className='size-3' />
                    {t('series.source.upToDate')}
                  </p>
                ) : (
                  <>
                    <ul className='grid max-h-52 gap-0.5 overflow-y-auto'>
                      {check.missing.map((chapter) => (
                        <li key={chapter.name}>
                          <label className='flex items-center gap-1.5 rounded-lg px-2 py-1 text-[10px] hover:bg-foreground/[0.045]'>
                            <input
                              type='checkbox'
                              checked={picked.includes(chapter.name)}
                              onChange={() =>
                                setPicked((current) =>
                                  current.includes(chapter.name)
                                    ? current.filter((name) => name !== chapter.name)
                                    : [...current, chapter.name],
                                )
                              }
                            />
                            {chapter.name}
                          </label>
                        </li>
                      ))}
                    </ul>
                    <Button
                      type='button'
                      size='sm'
                      disabled={chosen.length === 0 || startingFetch}
                      aria-busy={startingFetch}
                      className='h-7 gap-1.5 text-[10px]'
                      onClick={() => void download().catch(() => undefined)}
                    >
                      <RefreshCw className='size-3' />
                      {t('series.source.download', { count: chosen.length })}
                    </Button>
                  </>
                )}
              </>
            )}

            <Button
              type='button'
              size='sm'
              variant='ghost'
              disabled={busy || running || settingSource}
              className='h-6 gap-1.5 text-[10px] text-muted-foreground'
              onClick={() => void setSource(null).catch(() => undefined)}
            >
              <Unlink className='size-3' />
              {t('series.source.unbind')}
            </Button>
          </>
        ) : (
          <>
            <p className='px-0.5 text-[9px] text-muted-foreground'>
              {t('series.source.unboundHint')}
            </p>
            <div className='flex gap-1'>
              <Input
                value={address}
                onChange={(event) => setAddress(event.target.value)}
                placeholder={t('series.source.addressPlaceholder')}
                aria-label={t('series.source.addressPlaceholder')}
                className='h-7 text-[10px]'
              />
              <Button
                type='button'
                size='sm'
                disabled={settingSource || !address.trim()}
                aria-busy={settingSource}
                className='h-7 shrink-0 gap-1.5 text-[10px]'
                onClick={() => void bind().catch(() => undefined)}
              >
                {settingSource ? (
                  <LoaderCircle className='size-3 animate-spin' />
                ) : (
                  <Link2 className='size-3' />
                )}
                {t('series.source.bind')}
              </Button>
            </div>
          </>
        )}
      </PopoverContent>
    </Popover>
  )
}

/** Chapter-level progress. One page's share of a whole chapter is too fine to read at a glance, so
 * the bar tracks chapters and the page count is spelled out next to it. */
function percent(index: number, total: number): number {
  if (total <= 0) return 0
  return Math.min(100, Math.round((index / total) * 100))
}