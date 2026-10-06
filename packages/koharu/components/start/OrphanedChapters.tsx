'use client'

import { LoaderCircle, Trash2, TriangleAlert } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { useDeleteOrphanedChapter, useOrphanedChapters } from '@/lib/queries'
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
import type { ChapterRef } from '@koharu/bridge/protocol'

/** Chapters that no series claims, offered for deletion.
 *
 *  This is deliberately not the old ungrouped list. That one treated a chapter without a series as a
 *  place to keep working; here it is debris -- an import that failed before writing its index, or a
 *  chapter whose index the user deleted by hand. There is nothing to open and nothing to translate,
 *  so the only action left is to clear it away. */
export function OrphanedChapters() {
  const { t } = useTranslation()
  const orphaned = useOrphanedChapters()
  const { deleteOrphan, deletingOrphan } = useDeleteOrphanedChapter()
  const [pending, setPending] = useState<ChapterRef | null>(null)
  const [working, setWorking] = useState(false)

  const orphans = orphaned.data ?? []
  // Nothing to say until something is actually wrong; a permanent row reading "0 orphans" would
  // be noise on a shelf that is otherwise about the series the user is working on.
  if (orphaned.isPending || orphans.length === 0) return null

  // The chapter directory holds nothing but a sequence number, so the series directory it sits in
  // is the only thing that tells one orphan apart from another.
  const identify = (reference: ChapterRef) => `${reference.series}/${reference.chapter}`

  const confirm = async () => {
    if (!pending) return
    setWorking(true)
    try {
      await deleteOrphan(pending)
      setPending(null)
    } finally {
      setWorking(false)
    }
  }

  return (
    <section className='mx-auto mt-10 w-full max-w-[960px]' aria-labelledby='orphaned-title'>
      <div className='flex items-center gap-2'>
        <TriangleAlert className='size-4 shrink-0 text-destructive' />
        <h2 id='orphaned-title' className='text-[12px] font-medium'>
          {t('shelf.orphanedTitle')}
        </h2>
        <span className='text-[10px] text-muted-foreground tabular-nums'>{orphans.length}</span>
        <p className='ml-2 text-[10px] leading-4 text-muted-foreground'>
          {t('shelf.orphanedDescription')}
        </p>
      </div>

      <ul className='mt-3 grid gap-1'>
        {orphans.map((orphan) => (
          <li
            key={identify(orphan)}
            className='flex items-center gap-2 rounded-lg bg-foreground/[0.03] px-2 py-1.5'
          >
            <span className='min-w-0 flex-1 truncate text-[11px]'>{identify(orphan)}</span>
            <AlertDialog
              open={pending === orphan}
              onOpenChange={(open) => !open && setPending(null)}
            >
              <AlertDialogTrigger
                render={
                  <Button
                    type='button'
                    size='icon-sm'
                    variant='ghost'
                    disabled={deletingOrphan}
                    aria-label={t('shelf.orphanedDelete', { name: identify(orphan) })}
                    className='size-7 shrink-0 text-muted-foreground hover:bg-destructive/10 hover:text-destructive'
                  />
                }
              >
                <Trash2 className='size-3.5' />
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>
                    {t('shelf.orphanedDeleteTitle', { name: identify(orphan) })}
                  </AlertDialogTitle>
                  <AlertDialogDescription>
                    {t('shelf.orphanedDeleteDescription')}
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
                    {t('shelf.orphanedDeleteConfirm')}
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </li>
        ))}
      </ul>
    </section>
  )
}
