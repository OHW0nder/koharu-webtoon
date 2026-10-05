'use client'

import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'

/** Ad band heights are source-image pixels and run into the tens of thousands, so this is a plain
 *  numeric input rather than a slider: slider ticks cannot resolve that range, and the user is
 *  typing what they measured on the page anyway. Shared by the settings panel and both import
 *  dialogs so the two never drift apart. */
export function AdBandField({
  label,
  value,
  clearLabel,
  disabled = false,
  onChange,
}: {
  label: string
  value: number
  clearLabel: string
  disabled?: boolean
  onChange: (value: number) => void
}) {
  return (
    <div className='flex items-center gap-2'>
      <span className='w-24 shrink-0 text-[10px] text-muted-foreground'>{label}</span>
      <Input
        type='number'
        min={0}
        step={1}
        value={value}
        disabled={disabled}
        aria-label={label}
        className='h-7 w-24 text-[11px] tabular-nums'
        onChange={(event) => onChange(toHeight(event.currentTarget.value))}
      />
      <Button
        type='button'
        size='sm'
        variant='ghost'
        disabled={disabled || value === 0}
        className='h-7 shrink-0 px-1.5 text-[10px] font-normal text-muted-foreground'
        onClick={() => onChange(0)}
      >
        {clearLabel}
      </Button>
    </div>
  )
}

/** The stored value is a height, so an empty or negative field reads as "no band on this end" —
 *  the same thing clearing it means. */
function toHeight(raw: string): number {
  const value = Number(raw)
  if (!Number.isFinite(value) || value <= 0) return 0
  return Math.round(value)
}
