'use client'

import { ArrowLeft, Download, LoaderCircle, Plus, Rows3, ScrollText } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { AdBandField } from '@/components/series/AdBandField'
import { SeriesSettings } from '@/components/series/SeriesSettings'
import { call } from '@/lib/backend'
import {
  useExportSeriesChapters,
  useImportSeriesChapter,
  useProcessSeriesChapters,
  useSeriesCandidates,
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
import { Button } from '@koharu/ui/components/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@koharu/ui/components/dropdown-menu'
import { Popover, PopoverContent, PopoverTrigger } from '@koharu/ui/components/popover'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Switch } from '@koharu/ui/components/switch'

/** The pipelines worth offering per batch. A full run is the common case; the rest exist for
 *  re-running one stage after a prompt change without redoing the rest. */
const PIPELINES: { id: string; operation: Operation }[] = [
  { id: 'full', operation: { operation: 'full' } },
  { id: 'translateOnly', operation: { operation: 'only', stage: 'translation' } },
  {
    id: 'throughOcr',
    operation: { operation: 'stages', stages: ['detection', 'ocr', 'translation'] },
  },
]

export function SeriesView({ id }: { id: string }) {
  const { t } = useTranslation()
  const showShelf = useKoharuStore((state) => state.showShelf)
  const series = useSeriesDetail(id)
  const candidates = useSeriesCandidates(id)
  const { importChapter, importingChapter } = useImportSeriesChapter(id)
  const { processChapters, processing } = useProcessSeriesChapters(id)
  const { exportChapters, exporting } = useExportSeriesChapters(id)
  const [selected, setSelected] = useState<string[]>([])
  const [run, setRun] = useState<SeriesRun | null>(null)

  const chapters = series.data?.chapters ?? []
  const busy = processing || exporting || importingChapter
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
  const start = (operation: Operation) => {
    setRun(null)
    void processChapters({ projects: targets, operation })
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
          candidates={candidates.data ?? []}
          scanning={candidates.isFetching}
          ad={series.data?.settings.ad ?? { head: 0, tail: 0 }}
          onImport={(name, kind, ad) => importChapter({ name, kind, ad })}
        />

        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                type='button'
                size='sm'
                variant='outline'
                disabled={busy || targets.length === 0}
                aria-busy={processing}
                className='h-7 gap-1.5 text-[10px]'
              />
            }
          >
            {processing ? (
              <LoaderCircle className='size-3 animate-spin' />
            ) : (
              <Rows3 className='size-3' />
            )}
            {t('series.translate')}
          </DropdownMenuTrigger>
          <DropdownMenuContent align='end' className='min-w-40 border border-border/50 p-0.5'>
            {PIPELINES.map((pipeline) => (
              <DropdownMenuItem
                key={pipeline.id}
                className='min-h-7 gap-1.5 px-1.5 py-0.5 text-[11px]'
                onClick={() => start(pipeline.operation)}
              >
                {t(`series.pipeline.${pipeline.id}`)}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>

        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                type='button'
                size='sm'
                variant='outline'
                disabled={busy || targets.length === 0}
                aria-busy={exporting}
                className='h-7 gap-1.5 text-[10px]'
              />
            }
          >
            {exporting ? (
              <LoaderCircle className='size-3 animate-spin' />
            ) : (
              <Download className='size-3' />
            )}
            {t('series.export')}
          </DropdownMenuTrigger>
          <DropdownMenuContent align='end' className='min-w-32 border border-border/50 p-0.5'>
            {(['cbz', 'png', 'psd'] as const).map((format) => (
              <DropdownMenuItem
                key={format}
                className='min-h-7 gap-1.5 px-1.5 py-0.5 text-[11px]'
                onClick={() =>
                  void exportChapters({ projects: targets, format }).catch(() => undefined)
                }
              >
                {format.toUpperCase()}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      </header>

      {run && (
        <div className='flex flex-wrap items-center gap-x-3 gap-y-0.5 border-b border-border/40 px-4 py-1.5 text-[10px] text-muted-foreground'>
          <span className='tabular-nums'>
            {t('series.run.summary', { matched: run.matched, injected: run.injected })}
          </span>
          {run.dropped > 0 && (
            <span className='tabular-nums'>{t('series.run.dropped', { count: run.dropped })}</span>
          )}
          {run.unsupported && (
            <span className='text-destructive'>{t('series.run.unsupported')}</span>
          )}
        </div>
      )}

      {chapters.length > 0 && (
        <div className='flex items-center gap-2 border-b border-border/40 px-4 py-1.5'>
          <label className='flex items-center gap-1.5 text-[10px] text-muted-foreground'>
            <input
              type='checkbox'
              checked={targets.length === chapters.length}
              onChange={(event) =>
                setSelected(event.target.checked ? chapters.map((chapter) => chapter.project) : [])
              }
            />
            {t('series.selectAll')}
          </label>
          {chosen.length > 0 && (
            <span className='text-[10px] text-muted-foreground'>
              {t('series.selectedCount', { count: chosen.length })}
            </span>
          )}
        </div>
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
                />
              ))}
            </ul>
          )}
        </div>
      </ScrollArea>
    </div>
  )
}

/** A popover rather than a dropdown, because the ad band fields need to be typed into and Base
 *  UI's menu typeahead swallows every character key. */
function ImportChapterDialog({
  busy,
  candidates,
  scanning,
  ad,
  onImport,
}: {
  busy: boolean
  candidates: { name: string; seq: number; files: number }[]
  scanning: boolean
  ad: AdBands
  onImport: (name: string, kind: ChapterKind, ad: AdBands | null) => Promise<unknown>
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [choice, setChoice] = useState<{ name: string; kind: ChapterKind } | null>(null)
  const [inherit, setInherit] = useState(true)
  const [heights, setHeights] = useState<AdBands>(ad)

  // Re-seeded on every open, so an edit in the settings panel cannot rewrite what is being typed.
  const reopen = (next: boolean) => {
    if (next) {
      setChoice(null)
      setInherit(true)
      setHeights(ad)
    }
    setOpen(next)
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
            className='h-7 gap-1.5 text-[10px]'
          />
        }
      >
        <Plus className='size-3' />
        {t('series.importChapter')}
      </PopoverTrigger>
      <PopoverContent align='end' className='w-80 gap-2 p-2'>
        <div className='grid gap-1'>
          <p className='px-0.5 text-[10px] font-medium text-muted-foreground'>
            {t('series.importAd.chooseChapter')}
          </p>
          {candidates.length === 0 ? (
            <p className='px-0.5 text-[10px] text-muted-foreground'>
              {scanning ? t('common.loading') : t('series.noCandidates')}
            </p>
          ) : (
            candidates.map((candidate) => (
              <div key={candidate.name} className='grid gap-0.5'>
                <p className='px-0.5 text-[9px] text-muted-foreground'>
                  {candidate.name} · {t('series.fileCount', { count: candidate.files })}
                </p>
                <div className='flex gap-1'>
                  {(['webtoon', 'manga'] as const).map((kind) => {
                    const picked = choice?.name === candidate.name && choice.kind === kind
                    return (
                      <Button
                        key={kind}
                        type='button'
                        size='sm'
                        variant={picked ? 'secondary' : 'ghost'}
                        aria-pressed={picked}
                        className='h-7 flex-1 gap-1.5 text-[10px] font-normal'
                        onClick={() => setChoice({ name: candidate.name, kind })}
                      >
                        {kind === 'webtoon' ? (
                          <Rows3 className='size-3' />
                        ) : (
                          <ScrollText className='size-3' />
                        )}
                        {t(`series.kind.${kind}`)}
                      </Button>
                    )
                  })}
                </div>
              </div>
            ))
          )}
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
            disabled={busy || !choice}
            className='h-7 gap-1.5 text-[10px]'
            onClick={() => {
              if (!choice) return
              setOpen(false)
              void onImport(choice.name, choice.kind, inherit ? null : heights).catch(
                () => undefined,
              )
            }}
          >
            {busy ? <LoaderCircle className='size-3 animate-spin' /> : <Plus className='size-3' />}
            {t('series.importAd.confirm')}
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
}) {
  const { t } = useTranslation()
  const [opening, setOpening] = useState(false)

  const open = async () => {
    if (opening) return
    setOpening(true)
    try {
      await call(commands.openProject, chapter.project)
    } finally {
      setOpening(false)
    }
  }

  return (
    <li className='flex min-w-0 items-center gap-2 rounded-lg px-2 hover:bg-foreground/[0.045]'>
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
    </li>
  )
}
