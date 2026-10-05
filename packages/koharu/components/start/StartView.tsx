'use client'

import { BookOpen, LoaderCircle, Plus, Rows3, ScrollText, Settings } from 'lucide-react'
import { useTranslation } from 'react-i18next'

import { useImportSeries, useSeries } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import type { SeriesSummary } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@koharu/ui/components/dropdown-menu'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Tooltip, TooltipContent, TooltipTrigger } from '@koharu/ui/components/tooltip'

import { UngroupedProjects } from './UngroupedProjects'

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
              onImport={async (kind) => {
                const created = await importSeries(kind)
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

      <UngroupedProjects />
    </ScrollArea>
  )
}

function ImportMenu({
  importing,
  onImport,
}: {
  importing: boolean
  onImport: (kind: 'manga' | 'webtoon') => Promise<void>
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
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align='end'
        className='w-auto min-w-44 border border-border/50 p-0.5 shadow-sm ring-0'
      >
        <DropdownMenuItem
          disabled={importing}
          className='min-h-7 gap-1.5 px-1.5 py-0.5 text-[11px] [&_svg:not([class*="size-"])]:size-3.5'
          onClick={() => void onImport('manga')}
        >
          <ScrollText />
          {t('shelf.importManga')}
        </DropdownMenuItem>
        <DropdownMenuItem
          disabled={importing}
          className='min-h-7 gap-1.5 px-1.5 py-0.5 text-[11px] [&_svg:not([class*="size-"])]:size-3.5'
          onClick={() => void onImport('webtoon')}
        >
          <Rows3 />
          {t('shelf.importWebtoon')}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
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
