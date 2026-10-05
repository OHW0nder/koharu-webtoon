'use client'

import { FolderOpen, Plus, Trash2 } from 'lucide-react'
import { useCallback, useEffect, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { pageKey, pagesKey, projectKey, refresh } from '@/lib/queries'
import { commands, type ProjectSummary } from '@koharu/bridge/protocol'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogMedia,
  AlertDialogTitle,
} from '@koharu/ui/components/alert-dialog'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'

/**
 * Projects that no series claims. The flat list used to be the whole start screen; it now lives
 * here so a project created outside the shelf flow is still reachable and deletable.
 */
export function UngroupedProjects() {
  const { t } = useTranslation()
  const [projects, setProjects] = useState<ProjectSummary[]>([])
  const [name, setName] = useState('')
  const [busy, setBusy] = useState<string | null>('list')
  const [pending, setPending] = useState<string | null>(null)

  const reload = useCallback(async () => {
    setBusy('list')
    try {
      setProjects(await call(commands.listProjects))
    } finally {
      setBusy(null)
    }
  }, [])

  useEffect(() => {
    void reload().catch(() => undefined)
  }, [reload])

  const open = async (projectName: string) => {
    if (busy) return
    setBusy(projectName)
    try {
      await call(commands.openProject, projectName)
      await refresh(projectKey, pagesKey, pageKey)
    } finally {
      setBusy(null)
    }
  }

  const destroy = async () => {
    if (busy !== null || pending === null) return
    setBusy(pending)
    try {
      await call(commands.deleteProject, pending)
      setPending(null)
      await reload()
    } finally {
      setBusy(null)
    }
  }

  const create = async (event: FormEvent) => {
    event.preventDefault()
    const projectName = name.trim()
    if (!projectName || busy) return
    setBusy('create')
    try {
      await call(commands.createProject, projectName)
      // Creating a project also opens it, so the shelf has to pick up the new active project or
      // the editor never appears.
      await refresh(projectKey, pagesKey, pageKey)
      setName('')
    } finally {
      await reload()
      setBusy(null)
    }
  }

  return (
    <section
      className='mx-auto mt-12 w-full max-w-[960px] border-t border-border/60 pt-6'
      aria-labelledby='ungrouped-title'
    >
      <header className='flex flex-wrap items-center gap-2'>
        <h2 id='ungrouped-title' className='text-[12px] font-semibold'>
          {t('shelf.ungrouped')}
        </h2>
        <span className='rounded-full bg-muted px-2 py-0.5 text-[9px] text-muted-foreground tabular-nums'>
          {projects.length}
        </span>
        <form className='ml-auto flex items-center gap-1.5' onSubmit={create}>
          <Input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder={t('shelf.blankPlaceholder')}
            aria-label={t('shelf.blankPlaceholder')}
            autoComplete='off'
            disabled={busy !== null}
            className='h-7 w-40 bg-background text-[10px]'
          />
          <Button
            type='submit'
            size='sm'
            variant='outline'
            className='h-7 gap-1 text-[10px]'
            disabled={!name.trim() || busy !== null}
          >
            <Plus className='size-3' />
            {t('shelf.newBlank')}
          </Button>
        </form>
      </header>

      {projects.length === 0 ? (
        <p role='status' className='mt-4 text-[10px] text-muted-foreground'>
          {busy === 'list' ? t('common.loading') : t('shelf.ungroupedEmpty')}
        </p>
      ) : (
        <ul className='mt-3 grid gap-0.5'>
          {projects.map((project) => (
            <li
              key={project.name}
              className='group flex min-w-0 items-center rounded-lg hover:bg-foreground/[0.045]'
            >
              <button
                type='button'
                className='flex min-w-0 flex-1 items-center gap-3 rounded-lg px-3 py-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset'
                onClick={() => void open(project.name).catch(() => undefined)}
                disabled={busy !== null}
              >
                <span className='grid size-7 shrink-0 place-items-center rounded-lg bg-accent text-accent-foreground'>
                  <FolderOpen className='size-3.5' />
                </span>
                <span className='truncate text-[11px] font-medium'>{project.name}</span>
              </button>
              <Button
                type='button'
                size='icon-sm'
                variant='ghost'
                className='mr-2 size-7 shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100 hover:text-destructive focus-visible:opacity-100'
                aria-label={t('start.deleteLabel', { name: project.name })}
                onClick={() => setPending(project.name)}
                disabled={busy !== null}
              >
                <Trash2 className='size-3.5' />
              </Button>
            </li>
          ))}
        </ul>
      )}

      <AlertDialog
        open={pending !== null}
        onOpenChange={(open) => {
          if (!open && busy === null) setPending(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogMedia className='bg-destructive/10 text-destructive'>
              <Trash2 className='size-5' />
            </AlertDialogMedia>
            <AlertDialogTitle>{t('start.deleteTitle')}</AlertDialogTitle>
            <AlertDialogDescription>
              {t('start.deleteDescription', { name: pending })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={busy !== null}>{t('common.cancel')}</AlertDialogCancel>
            <AlertDialogAction
              variant='destructive'
              disabled={busy !== null}
              aria-busy={busy !== null}
              onClick={() => void destroy().catch(() => undefined)}
            >
              {busy !== null ? t('start.deleting') : t('start.deleteAction')}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  )
}
