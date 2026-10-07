import type { ChapterRef } from '@koharu/bridge/protocol'

/** A chapter's address on disk. The chapter directory holds nothing but a sequence number, so the
 *  series directory it sits in is what makes a reference addressable. */
export function chapterRef(series: string, chapter: { chapter: string }): ChapterRef {
  return { series, chapter: chapter.chapter }
}

/** The span a pair of range fields names, or `null` while they do not name one.
 *
 *  The end may be left empty: an open end means the single chapter the start names, which is the
 *  common "just this one" case. The ends are read unordered, so a reversed pair still selects what
 *  it obviously meant instead of nothing. */
export function chapterRange(from: string, to: string): { low: number; high: number } | null {
  const start = Number.parseInt(from, 10)
  if (!Number.isFinite(start)) return null
  const end = to.trim() === '' ? start : Number.parseInt(to, 10)
  if (!Number.isFinite(end)) return null
  return { low: Math.min(start, end), high: Math.max(start, end) }
}

/** The chapters a span covers, in shelf order.
 *
 *  A deleted chapter leaves its number free instead of shifting the rest, so a span skips those
 *  gaps rather than quietly covering a different set than the numbers suggest. */
export function chaptersInRange(
  chapters: { seq: number; chapter: string }[],
  series: string,
  range: { low: number; high: number },
): ChapterRef[] {
  return chapters
    .filter((chapter) => chapter.seq >= range.low && chapter.seq <= range.high)
    .map((chapter) => chapterRef(series, chapter))
}