'use client'

import { Download, LoaderCircle, Rows3, ScrollText, Search, Trash2 } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { useDeleteSeriesChapter, useExportSeriesChapters, useSeriesDetail } from '@/lib/queries'
import { useJobsRunning, useKoharuStore } from '@/lib/store'
import {
  commands,
  type ChapterRef,
  type ChapterStatus,
  type SeriesChapter,
} from '@koharu/bridge/protocol'
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
import { Input } from '@koharu/ui/components/input'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { cn } from '@koharu/ui/lib/utils'

/** How a chapter's state is told apart at a glance. Pending is the weakest signal because it is the
 *  one nothing has to be done about yet — a chapter sits there until the import lands. */
const STATUS_TONE: Record<ChapterStatus, string> = {
  done: 'bg-primary/12 text-primary',
  ready: 'bg-secondary text-secondary-foreground',
  pending: 'bg-muted text-muted-foreground',
}

/** The series' chapters, and the actions that address a selection of them.
 *
 *  Selection lives here rather than in the view above because only this pane can act on it: exporting
 *  and deleting both need the chosen chapters, and a selection that outlived the pane would have
 *  nothing left to mean. */
export function ChapterPane({ series }: { series: string }) {
  const { t } = useTranslation()
  const detail = useSeriesDetail(series)
  const showChapter = useKoharuStore((state) => state.showChapter)
  const running = useJobsRunning()
  const { exportChapters, exporting } = useExportSeriesChapters(series)
  const { deleteChapter, deletingChapter } = useDeleteSeriesChapter(series)
  const [query, setQuery] = useState('')
  const [selected, setSelected] = useState<string[]>([])

  // Held through a memo so the selection and the filter below only run when the index or the query
  // actually changed: the `??` fallback allocates a fresh array on every render otherwise.
  const chapters = useMemo(() => detail.data?.chapters ?? [], [detail.data])
  const busy = exporting || deletingChapter || running

  // Matching on both the display title and the folder name matters: chapters are imported from
  // whatever the downloader called them, so `#12` finds `Ch12` and a title finds both.
  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase()
    if (needle === '') return chapters
    return chapters.filter(
      (chapter) =>
        chapter.title.toLowerCase().includes(needle) ||
        chapter.chapter.toLowerCase().includes(needle) ||
        `#${chapter.seq}` === needle,
    )
  }, [chapters, query])

  const chosen = useMemo(
    () => chapters.filter((chapter) => selected.includes(chapter.chapter)),
    [chapters, selected],
  )
  // Select-all means what it covers on screen. The filter narrows the list, so the checkbox follows it
  // rather than the other way round — otherwise a search would silently redefine what it selects.
  const pickedVisible = visible.filter((chapter) => selected.includes(chapter.chapter)).length
  const allPicked = visible.length > 0 && pickedVisible === visible.length

  const toggle = (chapter: string) =>
    setSelected((current) =>
      current.includes(chapter)
        ? current.filter((entry) => entry !== chapter)
        : [...current, chapter],
    )

  return (
    <div className='flex h-full min-h-0 flex-col'>
      <div className='flex shrink-0 items-center gap-2 border-b border-border/40 px-3 py-2'>
        <div className='relative min-w-0 flex-1'>
          <Search className='pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground' />
          <Input
            value={query}
            placeholder={t('series.chapters.search')}
            aria-label={t('series.chapters.search')}
            className='h-7 pl-7 text-[11px]'
            onChange={(event) => setQuery(event.currentTarget.value)}
          />
        </div>
        <span className='shrink-0 text-[10px] text-muted-foreground tabular-nums'>
          {t('series.chapterCount', { count: visible.length })}
        </span>
      </div>

      <div className='flex shrink-0 items-center gap-2 border-b border-border/40 px-3 py-1.5'>
        <SelectAll
          allPicked={allPicked}
          somePicked={pickedVisible > 0 && !allPicked}
          disabled={busy || visible.length === 0}
          onToggle={() =>
            setSelected((current) =>
              allPicked
                ? current.filter((entry) => !visible.some((chapter) => chapter.chapter === entry))
                : [...new Set([...current, ...visible.map((chapter) => chapter.chapter)])],
            )
          }
        />

        {chosen.length > 0 && (
          <span className='text-[10px] text-muted-foreground tabular-nums'>
            {t('series.selectedCount', { count: chosen.length })}
          </span>
        )}

        <div className='ml-auto flex items-center gap-1'>
          <Button
            type='button'
            size='sm'
            variant='outline'
            disabled={busy || chosen.length === 0}
            aria-busy={exporting}
            className='h-7 gap-1.5 text-[10px]'
            onClick={() =>
              void exportChapters({
                chapters: chosen.map((chapter) => referenceOf(series, chapter)),
              }).catch(() => undefined)
            }
          >
            {exporting ? (
              <LoaderCircle className='size-3 animate-spin' />
            ) : (
              <Download className='size-3' />
            )}
            {t('series.export')}
          </Button>

          {chosen.length > 0 && (
            <DeleteChaptersDialog
              series={series}
              chapters={chosen}
              trigger={
                <Button
                  type='button'
                  size='sm'
                  variant='ghost'
                  disabled={busy}
                  className='h-7 gap-1.5 text-[10px] text-destructive hover:bg-destructive/10 hover:text-destructive'
                >
                  <Trash2 className='size-3' />
                  {t('series.deleteSelected', { count: chosen.length })}
                </Button>
              }
              onConfirm={async (chapters) => {
                for (const chapter of chapters) await deleteChapter(chapter)
                setSelected([])
              }}
            />
          )}
        </div>
      </div>

      <ScrollArea className='min-h-0 flex-1' viewportClassName='p-1.5'>
        {detail.isError ? (
          <p role='status' className='px-1.5 py-2 text-[11px] text-destructive'>
            {String(detail.error)}
          </p>
        ) : detail.isPending ? (
          <p role='status' className='px-1.5 py-2 text-[11px] text-muted-foreground'>
            {t('common.loading')}
          </p>
        ) : visible.length === 0 ? (
          <p
            role='status'
            className='grid min-h-32 place-items-center px-4 text-center text-[11px] leading-4 text-muted-foreground'
          >
            {chapters.length === 0 ? t('series.empty') : t('series.chapters.searchEmpty')}
          </p>
        ) : (
          <ul className='grid gap-0.5' aria-label={t('series.chapters.title')}>
            {visible.map((chapter) => (
              <ChapterRow
                key={chapter.chapter}
                series={series}
                chapter={chapter}
                picked={selected.includes(chapter.chapter)}
                disabled={busy}
                onToggle={() => toggle(chapter.chapter)}
                onDelete={() => deleteChapter(referenceOf(series, chapter))}
                onOpen={async (reference) => {
                  await call(commands.openChapter, reference)
                  showChapter({ seriesId: series, reference })
                }}
              />
            ))}
          </ul>
        )}
      </ScrollArea>
    </div>
  )
}

/** A chapter's address on disk. The chapter directory holds nothing but a sequence number, so the
 *  series directory it sits in is what makes a reference addressable. */
function referenceOf(series: string, chapter: { chapter: string }): ChapterRef {
  return { series, chapter: chapter.chapter }
}

/** Select-all over whatever the filter left visible. The mixed state is set through the DOM property
 *  rather than a prop — React has no `indeterminate` attribute — and it is the only way to say "some
 *  of what you see is selected" without lying about the count. */
function SelectAll({
  allPicked,
  somePicked,
  disabled,
  onToggle,
}: {
  allPicked: boolean
  somePicked: boolean
  disabled: boolean
  onToggle: () => void
}) {
  const { t } = useTranslation()
  const box = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (box.current) box.current.indeterminate = somePicked
  }, [somePicked])

  return (
    <label className='flex shrink-0 items-center gap-1.5 text-[10px] text-muted-foreground'>
      <input
        ref={box}
        type='checkbox'
        checked={allPicked}
        disabled={disabled}
        onChange={onToggle}
      />
      {t('series.selectAll')}
    </label>
  )
}

function ChapterRow({
  series,
  chapter,
  picked,
  disabled,
  onToggle,
  onDelete,
  onOpen,
}: {
  series: string
  chapter: SeriesChapter
  picked: boolean
  disabled: boolean
  onToggle: () => void
  onDelete: () => Promise<unknown>
  onOpen: (reference: ChapterRef) => Promise<void>
}) {
  const { t } = useTranslation()
  const [opening, setOpening] = useState(false)

  const open = async () => {
    if (opening) return
    setOpening(true)
    try {
      await onOpen(referenceOf(series, chapter))
    } finally {
      setOpening(false)
    }
  }

  return (
    <li
      data-picked={picked}
      className='group flex min-w-0 items-center gap-1 rounded-lg px-1.5 py-0.5 hover:bg-foreground/[0.045] data-[picked=true]:bg-accent/60'
    >
      <input
        type='checkbox'
        checked={picked}
        disabled={disabled}
        onChange={onToggle}
        aria-label={t('series.selectChapter', { name: chapter.title })}
        className='shrink-0'
      />
      <button
        type='button'
        onClick={() => void open()}
        disabled={opening}
        className='flex min-w-0 flex-1 items-center gap-2.5 rounded-lg py-1.5 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset'
      >
        {/* The slot number leads because it is the only part of the row that stays true after a
            deletion: a gap here means a chapter is missing, and the title says nothing about that. */}
        <span className='w-9 shrink-0 text-[10px] text-muted-foreground tabular-nums'>
          #{chapter.seq}
        </span>
        <span
          className={cn(
            'grid size-7 shrink-0 place-items-center rounded-md',
            chapter.status === 'done'
              ? 'bg-primary/12 text-primary'
              : 'bg-muted text-muted-foreground',
          )}
        >
          {opening ? (
            <LoaderCircle className='size-3.5 animate-spin' />
          ) : chapter.kind === 'webtoon' ? (
            <Rows3 className='size-3.5' />
          ) : (
            <ScrollText className='size-3.5' />
          )}
        </span>
        <span className='min-w-0 flex-1'>
          <span className='block truncate text-[11px] font-medium'>{chapter.title}</span>
          {/* The folder name only earns its line when it says something the title does not. Chapters
              imported from a folder are named after it, and then the two lines read identically. */}
          <span className='mt-0.5 block truncate text-[9px] text-muted-foreground'>
            {chapter.chapter === chapter.title
              ? t(`series.kind.${chapter.kind}`)
              : `${t(`series.kind.${chapter.kind}`)} · ${chapter.chapter}`}
          </span>
        </span>
        <span
          className={cn(
            'shrink-0 rounded-full px-1.5 py-0.5 text-[9px] leading-4',
            STATUS_TONE[chapter.status],
          )}
        >
          {t(`series.status.${chapter.status}`)}
        </span>
      </button>
      <DeleteChaptersDialog
        series={series}
        chapters={[chapter]}
        trigger={
          <Button
            type='button'
            size='icon-sm'
            variant='ghost'
            disabled={disabled}
            aria-label={t('series.deleteChapter', { name: chapter.title })}
            className='shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100 hover:bg-destructive/10 hover:text-destructive focus-visible:opacity-100'
          >
            <Trash2 className='size-3.5' />
          </Button>
        }
        onConfirm={() => onDelete().then(() => undefined)}
      />
    </li>
  )
}

/** Confirming a chapter deletion. Also used for the bulk case, where the same dialog names how many
 * chapters are about to go: a bulk delete confirmed only by a count is the one people click through. */
function DeleteChaptersDialog({
  series,
  chapters,
  trigger,
  onConfirm,
}: {
  series: string
  chapters: { title: string; chapter: string }[]
  trigger: React.ReactElement
  onConfirm: (chapters: ChapterRef[]) => Promise<void>
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [working, setWorking] = useState(false)

  const confirm = async () => {
    setWorking(true)
    try {
      await onConfirm(chapters.map((chapter) => referenceOf(series, chapter)))
      setOpen(false)
    } finally {
      setWorking(false)
    }
  }

  return (
    <AlertDialog open={open} onOpenChange={setOpen}>
      <AlertDialogTrigger render={trigger} />
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {chapters.length === 1
              ? t('series.deleteChapterTitle', { name: chapters[0]!.title })
              : t('series.deleteChaptersTitle', { count: chapters.length })}
          </AlertDialogTitle>
          <AlertDialogDescription>
            {chapters.length === 1
              ? t('series.deleteChapterDescription')
              : t('series.deleteChaptersDescription', { count: chapters.length })}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={working}>{t('common.cancel')}</AlertDialogCancel>
          <AlertDialogAction
            disabled={working}
            onClick={(event) => {
              event.preventDefault()
              void confirm()
            }}
          >
            {working && <LoaderCircle className='size-3 animate-spin' />}
            {t('series.deleteChapterConfirm')}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}