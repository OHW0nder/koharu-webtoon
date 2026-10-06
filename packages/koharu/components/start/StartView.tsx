'use client'

import { BookOpen, LoaderCircle, Plus, Rows3, ScrollText, Settings } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { AdBandField } from '@/components/series/AdBandField'
import { useImportSeries, useSeries } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import type { AdBands, ChapterKind, SeriesSummary } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Popover, PopoverContent, PopoverTrigger } from '@koharu/ui/components/popover'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Tooltip, TooltipContent, TooltipTrigger } from '@koharu/ui/components/tooltip'

import { OrphanedProjects } from './OrphanedProjects'

export function StartView() {
  const { t } = useTranslation()
  const setSettingsOpen = useKoharuStore((state) => state.setSettingsOpen)
  const showSeries = useKoharuStore((state) => state.showSeries)
  const series = useSeries()
  const shelves = series.data ?? []
  const { importSeries, importing } = useImportSeries()

  return (
    <ScrollArea className='min-h-0 flex-1' viewportClassName='p-6 sm:p-10'>
      <section className='mx-auto w-full max-w-[960px]' aria-labelledby='shelf-title'>
        <header className='flex items-start justify-between gap-6'>
          <div>
            <h1 id='shelf-title' className='text-[24px] font-semibold tracking-[-0.03em]'>
              {t('shelf.title')}
            </h1>
            <p className='mt-1 text-[12px] leading-5 text-muted-foreground'>
              {t('shelf.description')}
            </p>
          </div>
          <div className='flex shrink-0 items-center gap-1'>
            <ImportMenu
              importing={importing}
              onImport={async (kind, ad) => {
                const created = await importSeries({ kind, ad })
                if (created) showSeries(created.id)
              }}
            />
            <Tooltip>
              <TooltipTrigger
                render={
                  <Button
                    type='button'
                    variant='ghost'
                    size='icon-sm'
                    className='size-8 shrink-0 text-muted-foreground hover:bg-foreground/[0.05] hover:text-foreground'
                    aria-label={t('menu.settings')}
                    onClick={() => setSettingsOpen(true)}
                  />
                }
              >
                <Settings className='size-4' />
              </TooltipTrigger>
              <TooltipContent side='bottom'>{t('menu.settings')}</TooltipContent>
            </Tooltip>
          </div>
        </header>

        {shelves.length === 0 ? (
          series.isPending ? (
            <p
              role='status'
              className='mt-16 grid place-items-center text-[11px] text-muted-foreground'
            >
              {t('common.loading')}
            </p>
          ) : (
            <div className='mt-16 grid place-items-center text-center'>
              <span className='grid size-11 place-items-center rounded-xl bg-muted text-muted-foreground'>
                <BookOpen className='size-[18px]' />
              </span>
              <p className='mt-3 text-[12px] font-medium'>{t('shelf.emptyTitle')}</p>
              <p className='mt-1 text-[10px] leading-4 text-muted-foreground'>
                {t('shelf.emptyDescription')}
              </p>
            </div>
          )
        ) : (
          <ul className='mt-8 grid grid-cols-[repeat(auto-fill,minmax(150px,1fr))] gap-5'>
            {shelves.map((entry) => (
              <ShelfCard key={entry.id} entry={entry} onOpen={() => showSeries(entry.id)} />
            ))}
          </ul>
        )}
      </section>

      <OrphanedProjects />
    </ScrollArea>
  )
}

/** A popover rather than a dropdown, because the ad band fields have to be typed into and Base
 *  UI's menu typeahead swallows every character key.
 *
 *  The ad bands are asked for here rather than in the settings panel because this is the one
 *  moment the series does not exist: there is no index to keep the values in yet, so afterwards
 *  the settings panel owns them. */
function ImportMenu({
  importing,
  onImport,
}: {
  importing: boolean
  onImport: (kind: ChapterKind, ad: AdBands) => Promise<void>
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [kind, setKind] = useState<ChapterKind>('webtoon')
  const [ad, setAd] = useState<AdBands>({ head: 0, tail: 0 })

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        if (next) setAd({ head: 0, tail: 0 })
        setOpen(next)
      }}
    >
      <PopoverTrigger
        render={
          <Button
            type='button'
            size='sm'
            variant='outline'
            disabled={importing}
            aria-busy={importing}
            className='h-8 gap-1.5 rounded-lg text-[11px]'
          />
        }
      >
        {importing ? (
          <LoaderCircle className='size-3.5 animate-spin' />
        ) : (
          <Plus className='size-3.5' />
        )}
        {importing ? t('shelf.importing') : t('shelf.import')}
      </PopoverTrigger>
      <PopoverContent align='end' className='w-72 gap-2 p-2'>
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
          <p className='text-[10px] font-medium'>{t('series.ad.label')}</p>
          <AdBandField
            label={t('series.ad.head')}
            value={ad.head}
            clearLabel={t('series.ad.clearHead')}
            disabled={kind !== 'webtoon'}
            onChange={(head) => setAd((current) => ({ ...current, head }))}
          />
          <AdBandField
            label={t('series.ad.tail')}
            value={ad.tail}
            clearLabel={t('series.ad.clearTail')}
            disabled={kind !== 'webtoon'}
            onChange={(tail) => setAd((current) => ({ ...current, tail }))}
          />
          <p className='text-[9px] leading-4 text-muted-foreground'>
            {t('series.importAd.newSeriesHint')}
          </p>
          <Button
            type='button'
            size='sm'
            disabled={importing}
            className='h-7 gap-1.5 text-[10px]'
            onClick={() => {
              setOpen(false)
              // The argument is required either way, and ad bands only mean something for a
              // webtoon, so a page folder sends zeros.
              void onImport(kind, kind === 'webtoon' ? ad : { head: 0, tail: 0 })
            }}
          >
            {importing ? (
              <LoaderCircle className='size-3 animate-spin' />
            ) : (
              <Plus className='size-3' />
            )}
            {t('series.import.confirm')}
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  )
}

function ShelfCard({ entry, onOpen }: { entry: SeriesSummary; onOpen: () => void }) {
  const { t } = useTranslation()
  return (
    <li>
      <button
        type='button'
        onClick={onOpen}
        className='flex w-full flex-col gap-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset'
      >
        <span className='relative block aspect-[3/4] w-full overflow-hidden rounded-lg bg-muted'>
          <CoverPlaceholder className='size-full text-foreground' />
          {!entry.serial && (
            <span className='absolute top-1.5 left-1.5 rounded-full bg-background/85 px-1.5 py-0.5 text-[8px] text-muted-foreground'>
              {t('shelf.oneshot')}
            </span>
          )}
        </span>
        <span className='block min-w-0'>
          <span className='block truncate text-[11px] font-medium'>{entry.title}</span>
          <span className='mt-0.5 block text-[9px] text-muted-foreground tabular-nums'>
            {t('shelf.progress', { done: entry.done, total: entry.chapters })}
          </span>
        </span>
      </button>
    </li>
  )
}

/** Shown until a cover is chosen, so a shelf card never renders as an empty box. */
function CoverPlaceholder({ className }: { className?: string }) {
  return (
    <svg viewBox='0 0 60 80' className={className} role='presentation' aria-hidden='true'>
      <rect width='60' height='80' fill='currentColor' opacity='0.05' />
      <g stroke='currentColor' strokeWidth='1.5' strokeLinecap='round' opacity='0.28'>
        <path d='M16 22h28M16 30h28M16 38h18' />
        <path d='M16 54h28M16 62h20' />
      </g>
    </svg>
  )
}
