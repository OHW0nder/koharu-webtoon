'use client'

import { ChevronDown, FolderInput, LoaderCircle, Play, Plus, Rows3, ScrollText } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { AdBandField } from '@/components/series/AdBandField'
import { SourceUpdatePopover } from '@/components/series/SourceUpdatePopover'
import { useImportSeriesChapter, useProcessSeriesChapters, useSeriesDetail } from '@/lib/queries'
import { useJobsRunning } from '@/lib/store'
import {
  commands,
  type AdBands,
  type ChapterKind,
  type ChapterRef,
  type Operation,
  type SeriesRun,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@koharu/ui/components/dropdown-menu'
import { Input } from '@koharu/ui/components/input'
import { Popover, PopoverContent, PopoverTrigger } from '@koharu/ui/components/popover'
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

/** Everything that acts on the series as a whole: bringing chapters in, fetching the ones it does
 *  not have, and running a stage over a run of chapters.
 *
 *  It sits beside the chapter list rather than above it because these three have nothing to do with
 *  what the user is currently looking at — the import dialog asks for a folder, the fetcher asks for
 *  an address, and the run addresses a range typed into this panel. Selection-scoped actions stay
 *  with the chapters they select. */
export function SeriesActions({ series }: { series: string }) {
  const { t } = useTranslation()
  const detail = useSeriesDetail(series)
  const { importChapter, importingChapter } = useImportSeriesChapter(series)
  const { processChapters, processing } = useProcessSeriesChapters(series)
  const running = useJobsRunning()
  const [run, setRun] = useState<SeriesRun | null>(null)

  // A batch holds the project slot for every chapter it walks, so nothing that rewrites the index or
  // rebuilds a chapter may start while one is in flight.
  const busy = running || processing

  const chapters = useMemo(() => detail.data?.chapters ?? [], [detail.data])
  // The rule the backend applies when a downloaded chapter arrives: the most recently added chapter
  // decides. Stated here rather than left implicit so the popover can show it before the user acts.
  const latestKind =
    chapters.length === 0 ? null : chapters.reduce((a, b) => (b.seq > a.seq ? b : a)).kind

  // Injection is truncated silently on the backend, so the batch reports what actually reached
  // the prompt. Without that the user has no way to tell why the result keeps changing.
  const start = (targets: ChapterRef[], operation: Operation) => {
    setRun(null)
    void processChapters({ chapters: targets, operation })
      .then((result) => setRun(result))
      .catch(() => undefined)
  }

  return (
    <section
      aria-label={t('series.actions.title')}
      className='grid gap-3 rounded-xl border border-border/60 p-3'
    >
      <header className='grid gap-0.5'>
        <h2 className='text-[12px] font-semibold'>{t('series.actions.title')}</h2>
        <p className='text-[10px] leading-4 text-muted-foreground'>
          {t('series.actions.description')}
        </p>
      </header>

      <div className='grid gap-1.5'>
        <h3 className='text-[11px] font-medium'>{t('series.actions.sources')}</h3>
        <div className='flex flex-wrap gap-1.5'>
          <ImportChapterDialog
            ad={detail.data?.settings.ad ?? { head: 0, tail: 0 }}
            busy={busy || importingChapter}
            onImport={(directory, kind, ad) => importChapter({ directory, kind, ad })}
          />
          <SourceUpdatePopover
            series={series}
            source={detail.data?.source ?? null}
            kind={latestKind}
            busy={busy}
          />
        </div>
      </div>

      <div className='grid gap-1.5 border-t border-border/60 pt-3'>
        <h3 className='text-[11px] font-medium'>{t('series.actions.run')}</h3>
        {chapters.length > 0 ? (
          <ProcessRange
            series={series}
            chapters={chapters}
            busy={busy}
            running={processing}
            onRun={start}
          />
        ) : (
          <p className='text-[10px] leading-4 text-muted-foreground'>{t('series.range.empty')}</p>
        )}
        {run && (
          <div className='grid gap-0.5 text-[9px] leading-4 text-muted-foreground'>
            <span className='tabular-nums'>{t('series.run.summary', { matched: run.matched })}</span>
            {run.unsupported && <span className='text-destructive'>{t('series.run.unsupported')}</span>}
          </div>
        )}
      </div>
    </section>
  )
}

/** Runs one operation over a run of chapters, addressed the way the list shows them: `#4` through
 *  `#12`, with either end left open.
 *
 *  The backend takes chapter references and the index is already loaded here, so the range resolves against
 *  that list rather than teaching the command a second way to name the same chapters. A deleted
 *  chapter leaves its number free instead of shifting the rest, so a range skips those gaps rather than
 *  quietly running a different set than the numbers suggest. An empty end number means the single
 *  chapter the start names, which is the common "just this one" case.
 */
function ProcessRange({
  series,
  chapters,
  busy,
  running,
  onRun,
}: {
  series: string
  chapters: { seq: number; chapter: string }[]
  busy: boolean
  running: boolean
  onRun: (chapters: ChapterRef[], operation: Operation) => void
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

  const targets = useMemo(() => {
    if (!range) return []
    return chapters
      .filter((chapter) => chapter.seq >= range.low && chapter.seq <= range.high)
      .map((chapter) => ({ series, chapter: chapter.chapter }))
  }, [chapters, range, series])

  const status = !range
    ? t('series.range.hint')
    : targets.length === 0
      ? t('series.range.empty')
      : t('series.range.count', { count: targets.length })

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
      className='h-7 w-12 shrink-0 text-[11px] tabular-nums'
      onChange={(event) => onChange(event.currentTarget.value)}
    />
  )

  return (
    <div className='grid gap-1.5'>
      <div className='flex items-center gap-1.5'>
        {field(from, setFrom, t('series.range.from'))}
        <span className='text-[10px] text-muted-foreground'>–</span>
        {field(to, setTo, t('series.range.to'))}
      </div>
      <p className='text-[9px] leading-4 text-muted-foreground'>{status}</p>
      <DropdownMenu>
        <DropdownMenuTrigger
          disabled={busy || targets.length === 0}
          render={
            <Button
              type='button'
              size='sm'
              variant='outline'
              aria-busy={running}
              className='h-8 w-full justify-center gap-1.5 text-[11px]'
            />
          }
        >
          {running ? (
            <LoaderCircle className='size-3.5 animate-spin' />
          ) : (
            <Play className='size-3.5' />
          )}
          {t('series.processRun')}
          <ChevronDown className='size-3 opacity-60' />
        </DropdownMenuTrigger>
        <DropdownMenuContent align='end' className='min-w-40'>
          {PIPELINES.map((pipeline) => (
            <DropdownMenuItem
              key={pipeline.label}
              onClick={() => onRun(targets, pipeline.operation)}
            >
              {t(pipeline.label)}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
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
            className='h-8 gap-1.5 text-[11px]'
          />
        }
      >
        {working ? <LoaderCircle className='size-3.5 animate-spin' /> : <Plus className='size-3.5' />}
        {t('series.importChapter')}
      </PopoverTrigger>
      <PopoverContent align='start' className='w-80 gap-2 p-2'>
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