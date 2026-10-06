'use client'

import { useTranslation } from 'react-i18next'

import type { Operation, Stage } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'

/** Every stage the pipeline has, in the order it runs them. The batch and a single chapter offer
 *  the same set: a batch that cannot re-run one stage is a batch the user has to fall back to
 *  chapter-by-chapter for. */
export const STAGES: readonly Stage[] = ['detection', 'ocr', 'translation', 'inpainting'] as const

/** Turns a stage selection into the operation the backend takes.
 *
 *  All four is `full` because the backend already has that case and it reads as "everything" in the
 *  index; anything else lists the stages so a partial run stays a partial run. */
export function toOperation(stages: Stage[]): Operation {
  if (stages.length === STAGES.length) return { operation: 'full' }
  if (stages.length === 1) return { operation: 'only', stage: stages[0]! }
  return { operation: 'stages', stages }
}

/** The stage picker, shared by the single-chapter control and the batch dropdown.
 *
 *  Two copies would drift, and the user would end up with a "process" that means different things
 *  depending on which screen he is on. */
export function StagePicker({
  stages,
  onChange,
  disabled,
}: {
  stages: Stage[]
  onChange: (stages: Stage[]) => void
  disabled?: boolean
}) {
  const { t } = useTranslation()

  const toggle = (stage: Stage) =>
    onChange(
      stages.includes(stage) ? stages.filter((entry) => entry !== stage) : [...stages, stage],
    )

  return (
    <div className='grid gap-1'>
      <p className='px-0.5 text-[10px] font-medium text-muted-foreground'>
        {t('inference.pipelineStages')}
      </p>
      <div className='grid gap-0.5'>
        {STAGES.map((stage) => (
          <Button
            key={stage}
            type='button'
            size='sm'
            variant={stages.includes(stage) ? 'secondary' : 'ghost'}
            aria-pressed={stages.includes(stage)}
            disabled={disabled}
            className='h-7 justify-start gap-1.5 px-1.5 py-0.5 text-[11px] font-normal'
            onClick={() => toggle(stage)}
          >
            {t(`phase.${stage}`)}
          </Button>
        ))}
      </div>
      <div className='flex gap-1 border-t border-border/60 pt-1'>
        <Button
          type='button'
          size='sm'
          variant='ghost'
          disabled={disabled || stages.length === STAGES.length}
          className='h-7 flex-1 text-[10px] font-normal'
          onClick={() => onChange([...STAGES])}
        >
          {t('series.stageAll')}
        </Button>
        <Button
          type='button'
          size='sm'
          variant='ghost'
          disabled={disabled || stages.length === 0}
          className='h-7 flex-1 text-[10px] font-normal'
          onClick={() => onChange([])}
        >
          {t('series.stageNone')}
        </Button>
      </div>
    </div>
  )
}
