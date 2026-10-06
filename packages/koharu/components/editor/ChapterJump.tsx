'use client'

import { ChevronLeft, ChevronRight, List } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { useSeriesDetail } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Popover, PopoverContent, PopoverTrigger } from '@koharu/ui/components/popover'
import { ScrollArea } from '@koharu/ui/components/scroll-area'

/** Switching chapters without leaving the editor.
 *
 *  `open_project` replaces the active project in place — it stops the running job, resets the agent
 *  and republishes the canvas — so a jump is one command and there is no close-then-open round trip
 *  through the chapter list.
 *
 *  The badge shows the chapter number on its own rather than "n / total": the numbers are slots, and
 *  deleting a chapter leaves a hole, so "22 / 23" would read as "everything is translated" when it
 *  actually means a chapter is missing. The list below shows the real sequence. */
export function ChapterJump() {
  const { t } = useTranslation()
  const chapter = useKoharuStore((state) => state.chapter)
  const showChapter = useKoharuStore((state) => state.showChapter)
  const jobs = useKoharuStore((state) => state.jobs)
  const running = Object.values(jobs).some((job) => job.state === 'running')
  const series = useSeriesDetail(chapter?.seriesId ?? '', chapter !== null)
  const [open, setOpen] = useState(false)
  const [switching, setSwitching] = useState<string | null>(null)

  const chapters = series.data?.chapters ?? []
  const index = chapter ? chapters.findIndex((entry) => entry.project === chapter.project) : -1
  const current = index >= 0 ? chapters[index] : undefined
  const previous = index > 0 ? chapters[index - 1] : undefined
  const following = index >= 0 && index + 1 < chapters.length ? chapters[index + 1] : undefined

  // A chapter whose series is unknown is a project opened some other way. There is nothing to jump
  // to, so the control stays out of the way rather than showing an empty "0 / 0".
  if (!chapter || !current) return null

  const jump = async (project: string) => {
    if (switching) return
    setSwitching(project)
    try {
      await call(commands.openProject, project)
      setOpen(false)
    } finally {
      setSwitching(null)
    }
  }

  return (
    <div className='flex min-w-0 shrink-0 items-center gap-0.5'>
      <Button
        type='button'
        size='icon-sm'
        variant='ghost'
        disabled={!previous || switching !== null || running}
        aria-label={t('navigator.previousChapter')}
        onClick={() => previous && void jump(previous.project)}
        className='size-7 shrink-0 text-muted-foreground hover:bg-foreground/[0.05] hover:text-foreground'
      >
        <ChevronLeft className='size-4' />
      </Button>

      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger
          render={
            <Button
              type='button'
              size='sm'
              variant='ghost'
              disabled={switching !== null || running}
              className='h-7 min-w-0 gap-1.5 px-1.5 text-[10px] font-normal tabular-nums'
            />
          }
        >
          {switching !== null ? (
            <span className='size-3 animate-spin rounded-full border border-current border-t-transparent' />
          ) : (
            <List className='size-3.5 shrink-0 text-muted-foreground' />
          )}
          <span className='truncate'>{current.title}</span>
        </PopoverTrigger>
        <PopoverContent align='start' className='w-64 gap-1 p-1'>
          <p className='px-1.5 py-1 text-[10px] font-medium text-muted-foreground'>
            {series.data?.title ?? t('series.loading')}
          </p>
          {/* The height cap must sit on the viewport too: it is `size-full`, and a percentage height
              against an auto-height parent resolves to `auto`, which lets a long chapter list spill
              out of the popover instead of scrolling inside it. */}
          <ScrollArea className='min-h-0 max-h-72' viewportClassName='max-h-72'>
            <ul className='grid gap-0.5'>
              {chapters.map((entry, position) => (
                <li key={entry.project}>
                  <button
                    type='button'
                    disabled={switching !== null || running}
                    onClick={() => void jump(entry.project)}
                    className={`flex w-full items-center gap-2 rounded-md px-1.5 py-1.5 text-left text-[11px] transition-colors disabled:opacity-50 ${
                      entry.project === chapter.project
                        ? 'bg-accent text-accent-foreground'
                        : 'hover:bg-foreground/[0.05]'
                    }`}
                  >
                    <span className='w-7 shrink-0 text-[9px] text-muted-foreground tabular-nums'>
                      #{entry.seq}
                    </span>
                    <span className='min-w-0 flex-1 truncate'>{entry.title}</span>
                    {switching === entry.project && (
                      <span className='size-3 shrink-0 animate-spin rounded-full border border-current border-t-transparent' />
                    )}
                    <span className='shrink-0 text-[9px] text-muted-foreground tabular-nums'>
                      {position + 1}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </ScrollArea>
        </PopoverContent>
      </Popover>

      <Button
        type='button'
        size='icon-sm'
        variant='ghost'
        disabled={!following || switching !== null || running}
        aria-label={t('navigator.nextChapter')}
        onClick={() => following && void jump(following.project)}
        className='size-7 shrink-0 text-muted-foreground hover:bg-foreground/[0.05] hover:text-foreground'
      >
        <ChevronRight className='size-4' />
      </Button>
    </div>
  )
}
