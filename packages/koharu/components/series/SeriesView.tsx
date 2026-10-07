'use client'

import { ArrowLeft, LoaderCircle, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { ChapterPane } from '@/components/series/ChapterPane'
import { SeriesActions } from '@/components/series/SeriesActions'
import { SeriesSettings } from '@/components/series/SeriesSettings'
import { useDeleteSeries, useSeriesDetail } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@koharu/ui/components/alert-dialog'
import { Button } from '@koharu/ui/components/button'
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from '@koharu/ui/components/resizable'
import { ScrollArea } from '@koharu/ui/components/scroll-area'

/** One series, in two panes: the chapters on the left, everything that acts on or describes the
 *  whole series on the right.
 *
 *  The split is by ownership, not by size. The chapter list is the only thing here that grows without
 *  bound, so it takes the flexible pane; the settings describe one series and therefore have a
 *  ceiling — they scroll in a column of their own instead of pushing the chapter list off the page.
 *  Every panel reads the same series query and the same job state itself, so no `busy` flag has to
 *  travel down from here and stay in step with three independent actions. */
export function SeriesView({ id }: { id: string }) {
  const { t } = useTranslation()
  const showShelf = useKoharuStore((state) => state.showShelf)
  const { deleteSeries, deletingSeries } = useDeleteSeries()
  const series = useSeriesDetail(id)

  // Held through a memo so the counts below are not recomputed on every unrelated render: the `??`
  // fallback allocates a fresh array each time, which would defeat them and the panes' own memos.
  const chapters = useMemo(() => series.data?.chapters ?? [], [series.data])
  const done = useMemo(
    () => chapters.filter((chapter) => chapter.status === 'done').length,
    [chapters],
  )
  const webtoonChapters = useMemo(
    () => chapters.filter((chapter) => chapter.kind === 'webtoon').length,
    [chapters],
  )

  return (
    <div className='flex min-h-0 flex-1 flex-col bg-[var(--surface-canvas)]'>
      <header className='flex h-11 shrink-0 items-center gap-2 border-b border-border/50 px-3'>
        <Button
          type='button'
          size='icon-sm'
          variant='ghost'
          aria-label={t('series.backToShelf')}
          onClick={showShelf}
        >
          <ArrowLeft className='size-4' />
        </Button>
        <h1 className='min-w-0 truncate text-[13px] font-medium'>
          {series.data?.title ?? t('series.loading')}
        </h1>
        {!series.isPending && (
          <span className='shrink-0 rounded-full bg-secondary px-2 py-0.5 text-[10px] text-secondary-foreground tabular-nums'>
            {t('shelf.progress', { done, total: chapters.length })}
          </span>
        )}
        <DeleteSeriesButton
          busy={deletingSeries}
          onConfirm={() =>
            void deleteSeries(id)
              .then(() => showShelf())
              .catch(() => undefined)
          }
        />
      </header>

      <ResizablePanelGroup id='series' orientation='horizontal' className='min-h-0 flex-1'>
        {/* Both panes are keyed by id: switching series must clear the chapter filter and selection,
            which are held as chapter folder names and would otherwise carry over by name. */}
        <ResizablePanel
          id='chapters'
          defaultSize='58%'
          minSize='300px'
          className='min-h-0 min-w-0 overflow-hidden'
        >
          <ChapterPane key={id} series={id} />
        </ResizablePanel>
        <ResizableHandle className='w-0 bg-transparent' />
        <ResizablePanel
          id='settings'
          minSize='340px'
          className='min-h-0 min-w-0 overflow-hidden border-l border-border/40 bg-[var(--surface-panel)]'
        >
          <ScrollArea className='h-full' viewportClassName='p-3'>
            <div className='grid gap-3'>
              <SeriesActions series={id} />
              {/* Keyed for the same reason, plus the drafts inside are seeded once from the index. */}
              <SeriesSettings key={id} id={id} webtoonChapters={webtoonChapters} />
            </div>
          </ScrollArea>
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  )
}

/** The one series-level action left: removing the whole series.
 *
 *  Its own trigger rather than an overflow menu item, because a menu item closes the menu on click and
 *  the confirmation would race it. */
function DeleteSeriesButton({ busy, onConfirm }: { busy: boolean; onConfirm: () => void }) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [working, setWorking] = useState(false)

  const confirm = () => {
    setWorking(true)
    try {
      onConfirm()
      setOpen(false)
    } finally {
      setWorking(false)
    }
  }

  return (
    <AlertDialog open={open} onOpenChange={setOpen}>
      <AlertDialogTrigger
        render={
          <Button
            type='button'
            size='icon-sm'
            variant='ghost'
            disabled={busy}
            aria-label={t('series.deleteSeries')}
            className='ml-auto size-7 shrink-0 text-muted-foreground hover:bg-destructive/10 hover:text-destructive'
          />
        }
      >
        <Trash2 className='size-4' />
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t('series.deleteSeriesTitle')}</AlertDialogTitle>
          <AlertDialogDescription>{t('series.deleteSeriesDescription')}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={working}>{t('common.cancel')}</AlertDialogCancel>
          <AlertDialogAction
            disabled={working}
            onClick={(event) => {
              event.preventDefault()
              confirm()
            }}
          >
            {working && <LoaderCircle className='size-3 animate-spin' />}
            {t('series.deleteSeriesConfirm')}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}