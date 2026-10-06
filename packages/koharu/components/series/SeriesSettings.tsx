'use client'

import { Download, Plus, Trash2, Upload } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { AdBandField } from '@/components/series/AdBandField'
import {
  useGlossary,
  useSaveGlossary,
  useSaveSeriesSettings,
  useSeriesSettings,
} from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import type {
  AdBands,
  Glossary,
  GlossaryEntry,
  GlossaryKind,
  GlossaryValueOrigin,
  SeriesSettings,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Switch } from '@koharu/ui/components/switch'
import { Textarea } from '@koharu/ui/components/textarea'

/** The seven kinds a term can carry. Mirrors the backend enum; categories only filter and sort in
 *  the interface and never reach the prompt. */
const GLOSSARY_KINDS: GlossaryKind[] = [
  'person',
  'place',
  'organization',
  'item',
  'ability',
  'term',
  'other',
]

/** A debounced write that flushes on unmount.
 *
 *  Equality is decided on the serialized value, which is also what stops the debounce from
 *  re-firing: a rejected write leaves the payload equal to the one already submitted, so nothing
 *  is armed and the editor waits for the next edit. Without that, a failure the user cannot fix
 *  from the keyboard — a duplicated term, say — would retry on every render.
 *
 *  `enabled` is what the caller uses to hold the write off while the backend would reject it. It
 *  disarms the timer and silences the unmount flush, so leaving the page mid-lock cannot smuggle a
 *  rejected write out either.
 *
 *  The effect depends on the serialized string rather than the object so that an unrelated
 *  re-render cannot restart the timer and starve the save. */
function useDebouncedSave<T>({
  value,
  saving,
  enabled,
  submit,
}: {
  value: T | null
  saving: boolean
  enabled: boolean
  submit: (value: T) => Promise<unknown>
}) {
  const encoded = useMemo(() => (value === null ? null : JSON.stringify(value)), [value])
  const baseline = useRef<string | null>(null)
  const submitted = useRef<string | null>(null)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)
  const latest = useRef<T | null>(null)
  latest.current = value
  const latestSubmit = useRef(submit)
  latestSubmit.current = submit
  // Read through a ref so the unmount flush sees the current state rather than the first render's.
  const writing = useRef(saving)
  writing.current = saving
  const writable = useRef(enabled)
  writable.current = enabled

  const armed =
    enabled &&
    encoded !== null &&
    baseline.current !== null &&
    encoded !== baseline.current &&
    encoded !== submitted.current

  useEffect(() => {
    if (!armed || saving || encoded === null) return
    timer.current = setTimeout(() => {
      submitted.current = encoded
      void latestSubmit
        .current(latest.current as T)
        .then(() => {
          // Whatever was sent is now the stored truth, so it becomes the new baseline even if the
          // draft has moved on since.
          baseline.current = submitted.current
        })
        .catch(() => undefined)
    }, 350)
    return () => {
      clearTimeout(timer.current)
      timer.current = undefined
    }
  }, [armed, encoded, saving])

  // Leaving the page must not eat the edit that has not cleared the debounce yet.
  useEffect(
    () => () => {
      clearTimeout(timer.current)
      const pending = latest.current
      if (!writable.current || pending === null || writing.current) return
      const encoded = JSON.stringify(pending)
      if (encoded === baseline.current || encoded === submitted.current) return
      submitted.current = encoded
      void latestSubmit.current(pending).catch(() => undefined)
    },
    [],
  )

  /** Adopt the value that arrived from the backend as the stored truth, discarding nothing local. */
  const seed = useCallback((value: T) => {
    const encoded = JSON.stringify(value)
    baseline.current = encoded
    submitted.current = encoded
  }, [])

  return { seed, armed }
}

/** Series-scoped settings, on the chapter management page. The ad bands and the translation
 *  guidance are one index write and therefore one draft; the glossary is a separate file and gets
 *  its own writer below. */
export function SeriesSettings({ id, webtoonChapters }: { id: string; webtoonChapters: number }) {
  const { t } = useTranslation()
  const settings = useSeriesSettings(id)
  const { saveSettings, savingSettings } = useSaveSeriesSettings(id)
  const jobs = useKoharuStore((state) => state.jobs)
  // These writes are rejected while a job runs: the batch already snapshotted the series assets, so
  // a change now would not reach the running batch but would leave the interface describing a run
  // that is not happening.
  const running = Object.values(jobs).some((job) => job.state === 'running')
  const [draft, setDraft] = useState<SettingsDraft | null>(null)
  const seeded = useRef(false)

  useEffect(() => {
    // Seeded once: a later refetch must not overwrite edits the user is still typing.
    if (seeded.current || !settings.data) return
    seeded.current = true
    setDraft(settingsOf(settings.data))
  }, [settings.data])

  const { seed } = useDebouncedSave<SettingsDraft>({
    value: draft,
    saving: savingSettings,
    enabled: !running,
    submit: saveSettings,
  })
  useEffect(() => {
    if (settings.data) seed(settingsOf(settings.data))
  }, [seed, settings.data])

  return (
    <section
      aria-label={t('series.settings.title')}
      aria-busy={savingSettings}
      className='grid gap-3 rounded-xl border border-border/60 p-3'
    >
      <header className='grid gap-0.5'>
        <h2 className='text-[12px] font-semibold'>{t('series.settings.title')}</h2>
        <p className='text-[10px] leading-4 text-muted-foreground'>
          {t('series.settings.description')}
        </p>
        {running && <p className='text-[10px] text-muted-foreground'>{t('settings.busy')}</p>}
      </header>

      {!draft ? (
        <p className='text-[10px] text-muted-foreground'>{t('common.loading')}</p>
      ) : (
        <fieldset disabled={running} className='m-0 grid gap-3 border-0 p-0 disabled:opacity-60'>
          <div className='grid gap-4 md:grid-cols-2'>
            <div className='grid content-start gap-2'>
              <h3 className='text-[11px] font-medium'>{t('series.ad.label')}</h3>
              <AdBandField
                label={t('series.ad.head')}
                value={draft.ad.head}
                clearLabel={t('series.ad.clearHead')}
                onChange={(head) => setDraft({ ...draft, ad: { ...draft.ad, head } })}
              />
              <AdBandField
                label={t('series.ad.tail')}
                value={draft.ad.tail}
                clearLabel={t('series.ad.clearTail')}
                onChange={(tail) => setDraft({ ...draft, ad: { ...draft.ad, tail } })}
              />
              <p className='text-[9px] leading-4 text-muted-foreground'>{t('series.ad.hint')}</p>
              <p className='text-[9px] text-muted-foreground tabular-nums'>
                {t('series.ad.appliesTo', { count: webtoonChapters })}
              </p>
            </div>

            <div className='grid content-start gap-2'>
              <h3 className='text-[11px] font-medium'>{t('series.settings.guidance')}</h3>
              <Textarea
                value={draft.guidance}
                aria-label={t('series.settings.guidance')}
                placeholder={t('series.settings.guidancePlaceholder')}
                className='field-sizing-fixed max-h-48 min-h-20 resize-y overflow-y-auto text-[11px] leading-4'
                onChange={(event) => setDraft({ ...draft, guidance: event.currentTarget.value })}
              />
              <p className='text-[9px] leading-4 text-muted-foreground'>
                {t('series.settings.guidanceHint')}
              </p>
            </div>
          </div>

          <div className='grid content-start gap-2 border-t border-border/60 pt-3 md:grid-cols-2'>
            <h3 className='text-[11px] font-medium md:col-span-2'>{t('series.context.title')}</h3>
            <label className='flex items-center gap-2 text-[10px] text-muted-foreground'>
              <Input
                type='number'
                min={0}
                max={MAX_CONTEXT_PAGES}
                step={1}
                // Zero switches the feature off rather than setting a height, so it is a placeholder
                // and not text — otherwise the leading zero has to be deleted before typing a count.
                value={draft.context_pages === 0 ? '' : draft.context_pages}
                placeholder='0'
                aria-label={t('series.context.pages')}
                className='h-7 w-20 text-[11px] tabular-nums'
                onChange={(event) =>
                  setDraft({
                    ...draft,
                    context_pages: toPageCount(event.currentTarget.value),
                  })
                }
              />
              <span>{t('series.context.pages')}</span>
            </label>
            <p className='text-[9px] leading-4 text-muted-foreground'>
              {t('series.context.hint', { max: MAX_CONTEXT_PAGES })}
            </p>
          </div>

          <GlossaryPanel id={id} running={running} />
        </fieldset>
      )}
    </section>
  )
}

/** Mirrors the backend's own cap so the field cannot silently disagree with what the pipeline
 *  enforces. An index read back from an older release can carry more than this. */
const MAX_CONTEXT_PAGES = 12

/** The field is optional in the generated type because the backend marks it `serde(default)`, so
 *  an index written before it existed reads back as absent. The backend substitutes the same 4. */
const DEFAULT_CONTEXT_PAGES = 4

function settingsOf(settings: SeriesSettings): SettingsDraft {
  return {
    ad: settings.ad,
    guidance: settings.guidance,
    context_pages: Math.min(settings.context_pages ?? DEFAULT_CONTEXT_PAGES, MAX_CONTEXT_PAGES),
  }
}

/** The stored value is a distance in pages, so anything unparseable or negative reads as "no
 *  in-chapter context at all" — the same thing clearing it means. */
function toPageCount(raw: string): number {
  const value = Number(raw)
  if (!Number.isFinite(value) || value <= 0) return 0
  return Math.min(Math.round(value), MAX_CONTEXT_PAGES)
}

type SettingsDraft = { ad: AdBands; guidance: string; context_pages: number }

function GlossaryPanel({ id, running }: { id: string; running: boolean }) {
  const { t } = useTranslation()
  const glossary = useGlossary(id)
  const { saveGlossary, savingGlossary } = useSaveGlossary(id)
  const [draft, setDraft] = useState<Glossary | null>(null)
  // Two failures with two causes, so two messages: a rejected write is usually a term duplicated
  // within its category, while a rejected import is a file that was not a glossary at all.
  const [failure, setFailure] = useState<'save' | 'import' | null>(null)
  const seeded = useRef(false)
  const file = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (seeded.current || !glossary.data) return
    seeded.current = true
    setDraft(glossary.data)
  }, [glossary.data])

  // Rows without a source term are local placeholders. The backend rejects such an entry outright,
  // so keeping one in the payload would fail every other edit in the table.
  const payload = draft ? submittable(draft) : null
  const { seed } = useDebouncedSave<Glossary>({
    value: payload,
    saving: savingGlossary,
    enabled: !running,
    submit: async (glossary) => {
      setFailure(null)
      try {
        await saveGlossary(glossary)
      } catch {
        // Surfaced instead of retried: the debounce is disarmed by the failed payload, so the next
        // edit tries again rather than looping on the same rejected table.
        setFailure('save')
        throw new Error('the glossary was rejected')
      }
    },
  })
  useEffect(() => {
    if (glossary.data) seed(glossary.data)
  }, [glossary.data, seed])

  const update = (index: number, patch: Partial<GlossaryEntry>) =>
    setDraft((current) =>
      current
        ? {
            ...current,
            entries: current.entries.map((entry, at) =>
              at === index ? { ...entry, ...patch } : entry,
            ),
          }
        : current,
    )

  const remove = (index: number) =>
    setDraft((current) =>
      current ? { ...current, entries: current.entries.filter((_, at) => at !== index) } : current,
    )

  const exportFile = () => {
    if (!payload) return
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = `koharu-${id}-glossary.json`
    anchor.click()
    URL.revokeObjectURL(url)
  }

  const importFile = async (picked: File) => {
    let parsed: unknown
    try {
      parsed = JSON.parse(await picked.text())
    } catch {
      setFailure('import')
      return
    }
    const entries = (parsed as { entries?: unknown } | null)?.entries
    if (!Array.isArray(entries)) {
      setFailure('import')
      return
    }
    setFailure(null)
    setDraft((current) => {
      if (!current) return current
      const known = new Set(current.entries.map((entry) => entry.id))
      return {
        ...current,
        // Import replaces the list so that exporting and re-importing round-trips. Terms the user
        // has not seen before are marked imported; the rest keep the origin they had.
        entries: (entries as Partial<GlossaryEntry>[]).map((entry) =>
          normalizeEntry(entry, known.has(entry.id ?? '')),
        ),
      }
    })
  }

  return (
    <div className='grid gap-2'>
      <div className='flex flex-wrap items-center gap-1'>
        <h3 className='mr-1 text-[11px] font-medium'>{t('series.glossary.title')}</h3>
        <Button
          type='button'
          size='sm'
          variant='outline'
          disabled={!draft}
          className='h-7 gap-1.5 text-[10px]'
          onClick={() =>
            setDraft((current) =>
              current ? { ...current, entries: [...current.entries, newEntry()] } : current,
            )
          }
        >
          <Plus className='size-3' />
          {t('series.glossary.add')}
        </Button>
        <Button
          type='button'
          size='sm'
          variant='ghost'
          disabled={!draft}
          className='h-7 gap-1.5 px-1.5 text-[10px] text-muted-foreground'
          onClick={() => file.current?.click()}
        >
          <Upload className='size-3' />
          {t('series.glossary.import')}
        </Button>
        <Button
          type='button'
          size='sm'
          variant='ghost'
          disabled={!payload}
          className='h-7 gap-1.5 px-1.5 text-[10px] text-muted-foreground'
          onClick={exportFile}
        >
          <Download className='size-3' />
          {t('series.glossary.export')}
        </Button>
        <span className='ml-auto flex items-center gap-1.5'>
          <span className='text-[9px] text-muted-foreground tabular-nums'>
            {t('series.glossary.entryCount', { count: draft?.entries.length ?? 0 })}
          </span>
          {/* The table-level switch is what makes a glossary inject at all: a table nobody ever
              saved reads back disabled, so without this the terms could be edited forever and
              never reach a prompt. */}
          <span className='text-[9px] text-muted-foreground'>{t('series.glossary.enabled')}</span>
          <Switch
            size='sm'
            checked={draft?.enabled ?? false}
            disabled={!draft}
            aria-label={t('series.glossary.enabled')}
            onCheckedChange={(enabled) =>
              setDraft((current) => (current ? { ...current, enabled } : current))
            }
          />
        </span>
        <input
          ref={file}
          type='file'
          accept='application/json'
          className='hidden'
          onChange={(event) => {
            const picked = event.currentTarget.files?.[0]
            // Reset first so picking the same file twice still fires a change event.
            event.currentTarget.value = ''
            if (picked) void importFile(picked)
          }}
        />
      </div>

      {failure && (
        <p role='status' className='text-[9px] text-destructive'>
          {failure === 'save' ? t('series.glossary.rejected') : t('series.glossary.importFailed')}
        </p>
      )}

      {!draft ? (
        <p className='text-[10px] text-muted-foreground'>{t('common.loading')}</p>
      ) : draft.entries.length === 0 ? (
        <p className='text-[10px] text-muted-foreground'>{t('series.glossary.empty')}</p>
      ) : (
        <ScrollArea className='max-h-64' viewportClassName='p-1.5'>
          <ul className='grid gap-1.5'>
            {draft.entries.map((entry, index) => (
              <li key={entry.id} className='flex flex-wrap items-center gap-1.5'>
                <Input
                  value={entry.source}
                  aria-label={t('series.glossary.source')}
                  placeholder={t('series.glossary.source')}
                  className='h-7 min-w-32 flex-1 text-[11px]'
                  onChange={(event) => update(index, { source: event.currentTarget.value })}
                />
                <Input
                  value={entry.translation ?? ''}
                  aria-label={t('series.glossary.translation')}
                  placeholder={t('series.glossary.translation')}
                  className='h-7 min-w-32 flex-1 text-[11px]'
                  onChange={(event) =>
                    update(index, { translation: event.currentTarget.value || null })
                  }
                />
                <NativeSelect
                  size='sm'
                  value={entry.kind}
                  aria-label={t('series.glossary.category')}
                  className='w-28 shrink-0'
                  onChange={(event) =>
                    update(index, { kind: event.currentTarget.value as GlossaryKind })
                  }
                >
                  {GLOSSARY_KINDS.map((kind) => (
                    <NativeSelectOption key={kind} value={kind}>
                      {t(`series.glossary.kinds.${kind}`)}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
                <Switch
                  size='sm'
                  checked={entry.enabled}
                  aria-label={t('series.glossary.enabled')}
                  onCheckedChange={(enabled) => update(index, { enabled })}
                />
                <Input
                  value={entry.note}
                  aria-label={t('series.glossary.note')}
                  placeholder={t('series.glossary.notePlaceholder')}
                  className='h-7 min-w-32 flex-1 text-[11px]'
                  onChange={(event) => update(index, { note: event.currentTarget.value })}
                />
                {entry.source_origin !== 'user' && (
                  <span className='shrink-0 text-[9px] text-muted-foreground'>
                    {t(`series.glossary.origin.${entry.source_origin}`)}
                  </span>
                )}
                <Button
                  type='button'
                  size='icon-sm'
                  variant='ghost'
                  aria-label={t('series.glossary.remove', {
                    name: entry.source.trim() || t('series.glossary.untitled'),
                  })}
                  className='text-muted-foreground'
                  onClick={() => remove(index)}
                >
                  <Trash2 className='size-3.5' />
                </Button>
              </li>
            ))}
          </ul>
        </ScrollArea>
      )}
    </div>
  )
}

function submittable(glossary: Glossary): Glossary {
  return {
    ...glossary,
    entries: glossary.entries.filter((entry) => entry.source.trim().length > 0),
  }
}

/** A hand-written or exported file may miss fields or carry a kind outside the enum, so the gaps
 *  are filled in: one bad row must not make the whole table unsavable. */
function normalizeEntry(value: Partial<GlossaryEntry>, existed: boolean): GlossaryEntry {
  const origin: GlossaryValueOrigin = existed ? (value.source_origin ?? 'user') : 'imported'
  return {
    id: value.id || crypto.randomUUID(),
    source: value.source ?? '',
    translation: value.translation ?? null,
    kind: GLOSSARY_KINDS.includes(value.kind as GlossaryKind)
      ? (value.kind as GlossaryKind)
      : 'other',
    enabled: value.enabled ?? true,
    note: value.note ?? '',
    confidence: value.confidence ?? null,
    occurrence_count: value.occurrence_count ?? 0,
    examples: Array.isArray(value.examples) ? value.examples : [],
    source_origin: origin,
    translation_origin: existed ? (value.translation_origin ?? origin) : 'imported',
    present_in_last_scan: value.present_in_last_scan ?? false,
  }
}

function newEntry(): GlossaryEntry {
  return normalizeEntry({ enabled: true, source_origin: 'user' }, true)
}
