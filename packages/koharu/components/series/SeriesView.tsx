'use client'

import {
  ArrowLeft,
  ChevronDown,
  Download,
  FolderInput,
  LoaderCircle,
  Play,
  Plus,
  Rows3,
  ScrollText,
  Trash2,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { AdBandField } from '@/components/series/AdBandField'
import { SeriesSettings } from '@/components/series/SeriesSettings'
import { call } from '@/lib/backend'
import {
  useDeleteSeries,
  useDeleteSeriesChapter,
  useExportSeriesChapters,
  useImportSeriesChapter,
  useProcessSeriesChapters,
  useSeriesDetail,
} from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import {
  commands,
  type AdBands,
  type ChapterKind,
  type ChapterStatus,
  type Operation,
  type SeriesRun,
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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@koharu/ui/components/dropdown-menu'
import { Input } from '@koharu/ui/components/input'
import { Popover, PopoverContent, PopoverTrigger } from '@koharu/ui/components/popover'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Switch } from '@koharu/ui/components/switch'

/** What a range run can apply: the whole pipeline, or a single stage repeated across every chapter in
 *  the range. A plain list of single stages, the same shape it had before. A shared picker component
 *  was tried here and earned nothing: the per-chapter control has its own one, and the two never had
 *  to look alike. */
const PIPELINES: { label: string; operation: Operation }[] = [
  { label: 'series.pipeline.full', operation: { operation: 'full' } },
  { label: 'phase.detection', operation: { operation: 'only', stage: 'detection' } },
  { label: 'phase.ocr', operation: { operation: 'only', stage: 'ocr' } },
  { label: 'phase.translation', operation: { operation: 'only', stage: 'translation' } },
  { label: 'phase.inpainting', operation: { operation: 'only', stage: 'inpainting' } },
]

export function SeriesView({ id }: { id: string }) {
  const { t } = useTranslation()
  const showShelf = useKoharuStore((state) => state.showShelf)
  const showChapter = useKoharuStore((state) => state.showChapter)
  const series = useSeriesDetail(id)
  const { importChapter, importingChapter } = useImportSeriesChapter(id)
  const { processChapters, processing } = useProcessSeriesChapters(id)
  const { exportChapters, exporting } = useExportSeriesChapters(id)
  const { deleteChapter, deletingChapter } = useDeleteSeriesChapter(id)
  const { deleteSeries, deletingSeries } = useDeleteSeries()
  const [selected, setSelected] = useState<string[]>([])
  const [run, setRun] = useState<SeriesRun | null>(null)

  const chapters = series.data?.chapters ?? []
  const busy = processing || exporting || importingChapter || deletingChapter || deletingSeries
  const chosen = useMemo(
    () => chapters.filter((chapter) => selected.includes(chapter.project)),
    [chapters, selected],
  )
  const targets = chosen.map((chapter) => chapter.project)
  const webtoonChapters = chapters.filter((chapter) => chapter.kind === 'webtoon').length

  const toggle = (project: string) =>
    setSelected((current) =>
      current.includes(project)
        ? current.filter((entry) => entry !== project)
        : [...current, project],
    )

  // Injection is truncated silently on the backend, so the batch reports what actually reached
  // the prompt. Without that the user has no way to tell why the result keeps changing.
  const start = (projects: string[], operation: Operation) => {
    setRun(null)
    void processChapters({ projects, operation })
      .then((result) => setRun(result))
      .catch(() => undefined)
  }

  return (
    <div className='flex min-h-0 flex-1 flex-col bg-[var(--surface-canvas)]'>
      <header className='flex flex-wrap items-center gap-2 border-b border-border/50 px-4 py-3'>
        <Button
          type='button'
          size='icon-sm'
          variant='ghost'
          aria-label={t('series.backToShelf')}
          onClick={showShelf}
        >
          <ArrowLeft className='size-4' />
        </Button>
        <div className='min-w-0 flex-1'>
          <h1 className='truncate text-[13px] font-medium'>
            {series.data?.title ?? t('series.loading')}
          </h1>
          <p className='mt-0.5 text-[10px] text-muted-foreground'>
            {series.isPending ? '' : t('series.chapterCount', { count: chapters.length })}
          </p>
        </div>

        <ImportChapterDialog
          busy={busy}
          ad={series.data?.settings.ad ?? { head: 0, tail: 0 }}
          onImport={(directory, kind, ad) => importChapter({ directory, kind, ad })}
        />

        <Button
          type='button'
          size='sm'
          variant='outline'
          disabled={busy || targets.length === 0}
          aria-busy={exporting}
          className='h-7 gap-1.5 text-[10px]'
          onClick={() => void exportChapters({ projects: targets }).catch(() => undefined)}
        >
          {exporting ? (
            <LoaderCircle className='size-3 animate-spin' />
          ) : (
            <Download className='size-3' />
          )}
          {t('series.export')}
        </Button>

        <DeleteSeriesButton
          busy={busy}
          onConfirm={() =>
            void deleteSeries(id)
              .then(() => showShelf())
              .catch(() => undefined)
          }
        />
      </header>

      {run && (
        <div className='flex flex-wrap items-center gap-x-3 gap-y-0.5 border-b border-border/40 px-4 py-1.5 text-[10px] text-muted-foreground'>
          <span className='tabular-nums'>{t('series.run.summary', { matched: run.matched })}</span>
          {run.unsupported && (
            <span className='text-destructive'>{t('series.run.unsupported')}</span>
          )}
        </div>
      )}

      {chapters.length > 0 && (
        <>
          <div className='border-b border-border/40 px-4 py-1.5'>
            <ProcessRange
              chapters={chapters}
              busy={busy}
              running={processing}
              onRun={start}
            />
          </div>

          <div className='flex items-center gap-2 border-b border-border/40 px-4 py-1.5'>
            <label className='flex items-center gap-1.5 text-[10px] text-muted-foreground'>
              <input
                type='checkbox'
                checked={targets.length === chapters.length}
                onChange={(event) =>
                  setSelected(
                    event.target.checked ? chapters.map((chapter) => chapter.project) : [],
                  )
                }
              />
              {t('series.selectAll')}
            </label>
            {chosen.length > 0 && (
              <>
                <span className='text-[10px] text-muted-foreground'>
                  {t('series.selectedCount', { count: chosen.length })}
                </span>
                <DeleteChaptersDialog
                  chapters={chosen}
                  trigger={
                    <Button
                      type='button'
                      size='sm'
                      variant='ghost'
                      disabled={busy}
                      className='ml-auto h-6 gap-1 text-[10px] text-destructive hover:bg-destructive/10 hover:text-destructive'
                    >
                      <Trash2 className='size-3' />
                      {t('series.deleteSelected', { count: chosen.length })}
                    </Button>
                  }
                  onConfirm={async (projects) => {
                    for (const project of projects) await deleteChapter(project)
                    setSelected([])
                  }}
                />
              </>
            )}
          </div>
        </>
      )}

      <ScrollArea className='min-h-0 flex-1' viewportClassName='p-4'>
        <div className='mx-auto grid w-full max-w-3xl gap-4'>
          {/* Keyed by id so switching series re-seeds the settings drafts from the new index. */}
          <SeriesSettings key={id} id={id} webtoonChapters={webtoonChapters} />

          {series.isError ? (
            <p role='status' className='text-[11px] text-destructive'>
              {String(series.error)}
            </p>
          ) : chapters.length === 0 ? (
            <p
              role='status'
              className='grid min-h-32 place-items-center text-[11px] text-muted-foreground'
            >
              {series.isPending ? t('common.loading') : t('series.empty')}
            </p>
          ) : (
            <ul className='grid w-full gap-1'>
              {chapters.map((chapter) => (
                <ChapterRow
                  key={chapter.project}
                  chapter={chapter}
                  picked={selected.includes(chapter.project)}
                  disabled={busy}
                  onToggle={() => toggle(chapter.project)}
                  onDelete={() => deleteChapter(chapter.project)}
                  onOpen={async (project) => {
                    await call(commands.openProject, project)
                    showChapter({ seriesId: id, project })
                  }}
                />
              ))}
            </ul>
          )}
        </div>
      </ScrollArea>
    </div>
  )
}

/** Runs one operation over a run of chapters, addressed the way the list shows them: `#4` through
 *  `#12`, with either end left open.
 *
 *  The backend takes project names and the index is already loaded here, so the range resolves against
 *  that list rather than teaching the command a second way to name the same chapters. A deleted
 *  chapter leaves its number free instead of shifting the rest, so a range skips those gaps rather than
 *  quietly running a different set than the numbers suggest. An empty end number means the single
 *  chapter the start names, which is the common "just this one" case.
 */
function ProcessRange({
  chapters,
  busy,
  running,
  onRun,
}: {
  chapters: { seq: number; project: string }[]
  busy: boolean
  running: boolean
  onRun: (projects: string[], operation: Operation) => void
}) {
  const { t } = useTranslation()
  const [from, setFrom] = useState('')
  const [to, setTo] = useState('')

  const range = useMemo(() => {
    const start = Number.parseInt(from, 10)
    if (!Number.isFinite(start)) return null
    const end = to.trim() === '' ? start : Number.parseInt(to, 10)
    if (!Number.isFinite(end)) return null
    return { low: Math.min(start, end), high: Math.max(start, end) }
  }, [from, to])

  const projects = useMemo(() => {
    if (!range) return []
    return chapters
      .filter((chapter) => chapter.seq >= range.low && chapter.seq <= range.high)
      .map((chapter) => chapter.project)
  }, [chapters, range])

  const status = !range
    ? t('series.range.hint')
    : projects.length === 0
      ? t('series.range.empty')
      : t('series.range.count', { count: projects.length })

  const field = (value: string, onChange: (next: string) => void, label: string) => (
    <Input
      type='number'
      min={1}
      step={1}
      inputMode='numeric'
      value={value}
      disabled={busy}
      placeholder='#'
      aria-label={label}
      className='h-6 w-14 shrink-0 text-[10px] tabular-nums'
      onChange={(event) => onChange(event.currentTarget.value)}
    />
  )

  return (
    <div className='flex flex-wrap items-center gap-x-2 gap-y-1'>
      {field(from, setFrom, t('series.range.from'))}
      <span className='text-[10px] text-muted-foreground'>–</span>
      {field(to, setTo, t('series.range.to'))}
      <span className='text-[10px] text-muted-foreground'>{status}</span>

      <div className='ml-auto'>
        <DropdownMenu>
          <DropdownMenuTrigger
            disabled={busy || projects.length === 0}
            render={
              <Button
                type='button'
                size='sm'
                variant='outline'
                aria-busy={running}
                className='h-7 gap-1.5 text-[10px]'
              />
            }
          >
            {running ? (
              <LoaderCircle className='size-3 animate-spin' />
            ) : (
              <Play className='size-3' />
            )}
            {t('series.processRun')}
            <ChevronDown className='size-3 opacity-60' />
          </DropdownMenuTrigger>
          <DropdownMenuContent align='end'>
            {PIPELINES.map((pipeline) => (
              <DropdownMenuItem
                key={pipeline.label}
                onClick={() => onRun(projects, pipeline.operation)}
              >
                {t(pipeline.label)}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  )
}

/** Imports one chapter from a folder the user points at.
 *
 *  The flow mirrors what the user actually does: download a chapter, then hand the app that
 *  folder. There is no library of candidates to choose from, because there is no configured source
 *  folder — the download lands wherever the browser put it. The chapter name is the folder name, and
 *  only two things still need asking: the kind, which decides whether a tall image is cut into pages,
 *  and whether this once overrides the series' ad bands.
 */
function ImportChapterDialog({
  busy,
  ad,
  onImport,
}: {
  busy: boolean
  ad: AdBands
  onImport: (directory: string, kind: ChapterKind, ad: AdBands | null) => Promise<unknown>
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [kind, setKind] = useState<ChapterKind>('webtoon')
  const [inherit, setInherit] = useState(true)
  const [heights, setHeights] = useState<AdBands>(ad)
  const [working, setWorking] = useState(false)

  // Re-seeded on every open, so an edit in the settings panel cannot rewrite what is being typed.
  const reopen = (next: boolean) => {
    if (next) {
      setKind('webtoon')
      setInherit(true)
      setHeights(ad)
    }
    setOpen(next)
  }

  const pick = async () => {
    const picked = await commands.pickChapterFolder()
    if (!picked) return
    setOpen(false)
    setWorking(true)
    try {
      await onImport(picked, kind, inherit ? null : heights)
    } finally {
      setWorking(false)
    }
  }

  return (
    <Popover open={open} onOpenChange={reopen}>
      <PopoverTrigger
        render={
          <Button
            type='button'
            size='sm'
            variant='outline'
            disabled={busy || working}
            aria-busy={working}
            className='h-7 gap-1.5 text-[10px]'
          />
        }
      >
        {working ? <LoaderCircle className='size-3 animate-spin' /> : <Plus className='size-3' />}
        {t('series.importChapter')}
      </PopoverTrigger>
      <PopoverContent align='end' className='w-80 gap-2 p-2'>
        <div className='grid gap-1'>
          <p className='px-0.5 text-[10px] font-medium text-muted-foreground'>
            {t('series.import.kind')}
          </p>
          <div className='flex gap-1'>
            {(['manga', 'webtoon'] as const).map((candidate) => (
              <Button
                key={candidate}
                type='button'
                size='sm'
                variant={kind === candidate ? 'secondary' : 'ghost'}
                aria-pressed={kind === candidate}
                className='h-7 flex-1 gap-1.5 text-[10px] font-normal'
                onClick={() => setKind(candidate)}
              >
                {candidate === 'webtoon' ? (
                  <Rows3 className='size-3' />
                ) : (
                  <ScrollText className='size-3' />
                )}
                {t(`series.kind.${candidate}`)}
              </Button>
            ))}
          </div>
        </div>

        <div className='grid gap-1.5 border-t border-border/60 pt-2'>
          <div className='flex items-center justify-between gap-2'>
            <span className='text-[10px] text-muted-foreground'>
              {t('series.importAd.inherit')}
            </span>
            <Switch
              size='sm'
              checked={inherit}
              aria-label={t('series.importAd.inherit')}
              onCheckedChange={setInherit}
            />
          </div>

          {!inherit && (
            <div className='grid gap-1'>
              <AdBandField
                label={t('series.ad.head')}
                value={heights.head}
                clearLabel={t('series.ad.clearHead')}
                onChange={(head) => setHeights((current) => ({ ...current, head }))}
              />
              <AdBandField
                label={t('series.ad.tail')}
                value={heights.tail}
                clearLabel={t('series.ad.clearTail')}
                onChange={(tail) => setHeights((current) => ({ ...current, tail }))}
              />
            </div>
          )}

          <p className='text-[9px] leading-4 text-muted-foreground'>
            {inherit ? t('series.ad.hint') : t('series.importAd.inheritHint')}
          </p>

          <Button
            type='button'
            size='sm'
            disabled={busy || working}
            className='h-7 gap-1.5 text-[10px]'
            onClick={() => void pick()}
          >
            {working ? (
              <LoaderCircle className='size-3 animate-spin' />
            ) : (
              <FolderInput className='size-3' />
            )}
            {t('series.importAd.chooseFolder')}
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  )
}
function ChapterRow({
  chapter,
  picked,
  disabled,
  onToggle,
  onDelete,
  onOpen,
}: {
  chapter: {
    seq: number
    title: string
    project: string
    kind: ChapterKind
    status: ChapterStatus
  }
  picked: boolean
  disabled: boolean
  onToggle: () => void
  onDelete: () => Promise<unknown>
  onOpen: (project: string) => Promise<void>
}) {
  const { t } = useTranslation()
  const [opening, setOpening] = useState(false)

  const open = async () => {
    if (opening) return
    setOpening(true)
    try {
      await onOpen(chapter.project)
    } finally {
      setOpening(false)
    }
  }

  return (
    <li className='group flex min-w-0 items-center gap-2 rounded-lg px-2 hover:bg-foreground/[0.045]'>
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
        className='flex min-w-0 flex-1 items-center gap-3 rounded-lg py-2.5 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset'
      >
        <span className='grid size-8 shrink-0 place-items-center rounded-lg bg-accent text-accent-foreground'>
          {opening ? (
            <LoaderCircle className='size-4 animate-spin' />
          ) : chapter.kind === 'webtoon' ? (
            <Rows3 className='size-4' />
          ) : (
            <ScrollText className='size-4' />
          )}
        </span>
        <span className='min-w-0 flex-1'>
          <span className='block truncate text-[11px] font-medium'>{chapter.title}</span>
          <span className='mt-0.5 block truncate text-[9px] text-muted-foreground'>
            {t(`series.kind.${chapter.kind}`)} · {t(`series.status.${chapter.status}`)}
          </span>
        </span>
        <span className='shrink-0 text-[9px] text-muted-foreground'>#{chapter.seq}</span>
      </button>
      <DeleteChaptersDialog
        chapters={[chapter]}
        trigger={
          <Button
            type='button'
            size='icon-sm'
            variant='ghost'
            disabled={disabled}
            aria-label={t('series.deleteChapter', { name: chapter.title })}
            className='shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 hover:bg-destructive/10 hover:text-destructive focus-visible:opacity-100'
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
 *  chapters are about to go: a bulk delete confirmed only by a count is the one people click through. */
function DeleteChaptersDialog({
  chapters,
  trigger,
  onConfirm,
}: {
  chapters: { title: string; project: string }[]
  trigger: React.ReactElement
  onConfirm: (projects: string[]) => Promise<void>
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [working, setWorking] = useState(false)

  const confirm = async () => {
    setWorking(true)
    try {
      await onConfirm(chapters.map((chapter) => chapter.project))
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
            className='size-7 shrink-0 text-muted-foreground hover:bg-destructive/10 hover:text-destructive'
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
