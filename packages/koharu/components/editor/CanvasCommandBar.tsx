'use client'

// 章间跳转暂时不接线。跨章在内核里就是「换项目」，而内核同一时刻只允许持有一个项目：槽位是单数的
// （`CurrentProject`），每个项目还各带一把文件锁（`project.lock`）。所以换章只有两种结局：撞上自己
// 持有的锁，或者把用户正在编辑的那一章连同页码、选中图层一起换掉。跑批同样是逐章换项目，用户在看的
// 那一章一旦落进计划里，循环走到它就会以「已打开」中断整批。这条约束来自上游，不是扩展层能绕开的——
// 真要同时持有，得让内核支持并存的打开句柄，那是改上游而不是加扩展。
// 恢复这个控件之前要先定下批处理与用户编辑谁优先，并把章节列表页的跨章打开一并收口。
// import { ChapterJump } from '@/components/editor/ChapterJump'
import { InferenceControl } from '@/components/editor/InferenceControl'
import { call } from '@/lib/backend'
import { usePage } from '@/lib/queries'
import { pipelineStages, useKoharuStore, type PipelineScope } from '@/lib/store'
import { commands, type Scope, type Stage } from '@koharu/bridge/protocol'

export function CanvasCommandBar() {
  const page = usePage().data
  const selectedPages = useKoharuStore((state) => state.selectedPages)
  const jobs = useKoharuStore((state) => state.jobs)
  const running = Object.values(jobs).find((job) => job.state === 'running')

  const run = (selection: PipelineScope, stages: Stage[]) => {
    if (!page) return
    const scope: Scope =
      selection === 'project'
        ? { scope: 'project' }
        : selection === 'selected-pages'
          ? { scope: 'pages', value: selectedPages }
          : { scope: 'pages', value: [page.id] }
    const operation =
      stages.length === pipelineStages.length
        ? ({ operation: 'full' } as const)
        : stages.length === 1
          ? ({ operation: 'only', stage: stages[0]! } as const)
          : ({ operation: 'stages', stages } as const)
    void call(commands.process, scope, operation).catch(() => undefined)
  }

  return (
    <header className='flex h-10 shrink-0 items-center gap-2 border-b border-border/80 bg-[var(--surface-toolbar)] px-2.5'>
      <div className='min-w-0 flex-1' />
      <InferenceControl disabled={!page || Boolean(running)} onRun={run} />
    </header>
  )
}
