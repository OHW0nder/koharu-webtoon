'use client'

import { useTranslation } from 'react-i18next'

import { Input } from '@koharu/ui/components/input'

/** The two fields that address a run of chapters.
 *
 *  Chapter numbers reach three digits, so the fields are wider than a bare digit would need. The
 *  native number spinner is dropped: a range is typed rather than dialled, and the spinner claims
 *  the very width the digits need — with it in place, `107` showed as `10`. */
export function ChapterRangeFields({
  from,
  to,
  onFrom,
  onTo,
  disabled,
}: {
  from: string
  to: string
  onFrom: (next: string) => void
  onTo: (next: string) => void
  disabled?: boolean
}) {
  const { t } = useTranslation()

  const field = (value: string, onChange: (next: string) => void, label: string) => (
    <Input
      type='number'
      min={1}
      step={1}
      inputMode='numeric'
      value={value}
      disabled={disabled}
      placeholder='#'
      aria-label={label}
      className='h-7 w-14 shrink-0 [appearance:textfield] text-[11px] tabular-nums [&::-moz-number-spin-button]:appearance-none [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none'
      onChange={(event) => onChange(event.currentTarget.value)}
    />
  )

  return (
    <div className='flex items-center gap-1.5'>
      {field(from, onFrom, t('series.range.from'))}
      <span className='text-[10px] text-muted-foreground'>–</span>
      {field(to, onTo, t('series.range.to'))}
    </div>
  )
}