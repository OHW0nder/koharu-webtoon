'use client'

import { FolderOpen, LoaderCircle } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import {
  PreferencePage,
  PreferenceRow,
  PreferenceSection,
} from '@/components/preferences/PreferenceFields'
import { changeLibraryRoot, chooseLibraryFolder, fetchLibraryRoot } from '@/lib/backend'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'

/** Where every imported series is written.
 *
 *  The backend resolves the folder once at startup and holds it, so a change here only takes effect
 *  after a restart. The path is shown rather than edited inline because it is the one setting whose
 *  value the user cannot check by looking at the app: a wrong folder looks exactly like an empty
 *  library until they go and find it. The picker therefore does the choosing, and the field is only
 *  there to confirm what was chosen. */
export function LibraryPreferences() {
  const { t } = useTranslation()
  const [root, setRoot] = useState<string | null>(null)
  const [pending, setPending] = useState<string | null>(null)
  const [applying, setApplying] = useState(false)

  useEffect(() => {
    void fetchLibraryRoot()
      .then(setRoot)
      .catch(() => undefined)
  }, [])

  // Nothing to show until the real path is known: a placeholder would be read as the location.
  if (root === null) return null

  const browse = async () => {
    const picked = await chooseLibraryFolder()
    if (picked === null) return
    setPending(picked)
  }

  const apply = async () => {
    if (pending === null || pending === root) return
    setApplying(true)
    try {
      await changeLibraryRoot(pending)
      setRoot(pending)
      setPending(null)
    } finally {
      setApplying(false)
    }
  }

  const target = pending ?? root
  const dirty = pending !== null && pending !== root

  return (
    <PreferencePage
      title={t('settings.library.title')}
      description={t('settings.library.description')}
    >
      <PreferenceSection
        title={t('settings.library.location')}
        description={t('settings.library.locationDescription')}
      >
        <PreferenceRow
          title={t('settings.library.folder')}
          description={t('settings.library.folderDescription')}
          align='start'
        >
          <div className='grid gap-2'>
            <Input
              readOnly
              value={target}
              aria-label={t('settings.library.folder')}
              className='h-8 text-[12px] text-muted-foreground'
            />
            <div className='flex flex-wrap items-center gap-2'>
              <Button
                type='button'
                size='sm'
                variant='outline'
                disabled={applying}
                className='h-7 gap-1.5 text-[10px]'
                onClick={() => void browse().catch(() => undefined)}
              >
                <FolderOpen className='size-3' />
                {t('settings.library.browse')}
              </Button>
              {dirty && (
                <>
                  <Button
                    type='button'
                    size='sm'
                    disabled={applying}
                    className='h-7 text-[10px]'
                    onClick={() => void apply().catch(() => undefined)}
                  >
                    {applying && <LoaderCircle className='size-3 animate-spin' />}
                    {t('settings.library.apply')}
                  </Button>
                  <Button
                    type='button'
                    size='sm'
                    variant='ghost'
                    disabled={applying}
                    className='h-7 text-[10px]'
                    onClick={() => setPending(null)}
                  >
                    {t('common.cancel')}
                  </Button>
                </>
              )}
            </div>
            {dirty && (
              <p className='text-[10px] leading-4 text-muted-foreground'>
                {t('settings.library.restartNotice')}
              </p>
            )}
          </div>
        </PreferenceRow>
      </PreferenceSection>
    </PreferencePage>
  )
}
