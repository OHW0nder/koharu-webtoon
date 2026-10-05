'use client'

import { ArrowLeft, Download, LoaderCircle, Plus, Rows3, ScrollText } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

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
  type ChapterKind,
  type ChapterStatus,
  type Operation,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@koharu/ui/components/dropdown-menu'
import { ScrollArea } from '@koharu/ui/components/scroll-area'

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

  const chapters = series.data?.chapters ?? []
  const busy = processing || exporting || importingChapter
  const chosen = useMemo(
    () => chapters.filter((chapter) => selected.includes(chapter.project)),
    [chapters, selected],
  )
  const targets = chosen.map((chapter) => chapter.project)

  const toggle = (project: string) =>
    setSelected((current) =>
      current.includes(project)
        ? current.filter((entry) => entry !== project)
        : [...current, project],
    )

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

        <ImportChapterMenu
          busy={busy}
          candidates={candidates.data ?? []}
          scanning={candidates.isFetching}
          onImport={(name, kind) => importChapter({ name, kind })}
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
                onClick={() =>
                  void processChapters({ projects: targets, operation: pipeline.operation }).catch(
                    () => undefined,
                  )
                }
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
        {series.isError ? (
          <p role='status' className='text-[11px] text-destructive'>
            {String(series.error)}
          </p>
        ) : chapters.length === 0 ? (
          <p
            role='status'
            className='grid min-h-full place-items-center text-[11px] text-muted-foreground'
          >
            {series.isPending ? t('common.loading') : t('series.empty')}
          </p>
        ) : (
          <ul className='mx-auto grid w-full max-w-2xl gap-1'>
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
      </ScrollArea>
    </div>
  )
}

function ImportChapterMenu({
  busy,
  candidates,
  scanning,
  onImport,
}: {
  busy: boolean
  candidates: { name: string; seq: number; files: number }[]
  scanning: boolean
  onImport: (name: string, kind: ChapterKind) => Promise<unknown>
}) {
  const { t } = useTranslation()
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
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
      </DropdownMenuTrigger>
      <DropdownMenuContent align='end' className='min-w-52 border border-border/50 p-0.5'>
        {candidates.length === 0 ? (
          <p className='px-2 py-1.5 text-[10px] text-muted-foreground'>
            {scanning ? t('common.loading') : t('series.noCandidates')}
          </p>
        ) : (
          candidates.map((candidate) => (
            <div key={candidate.name}>
              <p className='px-2 pt-1.5 text-[9px] text-muted-foreground'>
                {candidate.name} · {t('series.fileCount', { count: candidate.files })}
              </p>
              {(['webtoon', 'manga'] as const).map((kind) => (
                <DropdownMenuItem
                  key={kind}
                  className='min-h-7 gap-1.5 px-1.5 py-0.5 text-[11px]'
                  onClick={() => void onImport(candidate.name, kind).catch(() => undefined)}
                >
                  {kind === 'webtoon' ? (
                    <Rows3 className='size-3.5' />
                  ) : (
                    <ScrollText className='size-3.5' />
                  )}
                  {t(`series.kind.${kind}`)}
                </DropdownMenuItem>
              ))}
            </div>
          ))
        )}
      </DropdownMenuContent>
    </DropdownMenu>
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
