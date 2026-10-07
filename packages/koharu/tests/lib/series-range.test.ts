import { describe, expect, it } from 'vitest'

import { chapterRange, chaptersInRange } from '@/lib/series-range'

/** The shelf a range resolves against. Chapter 11 is missing on purpose: a deleted chapter leaves
 *  its number free instead of shifting the rest, and a range has to skip the gap rather than
 *  quietly cover a different set than the numbers suggest. */
const SHELF = [
  { seq: 10, chapter: 'Ch10' },
  { seq: 12, chapter: 'Ch12' },
  { seq: 13, chapter: 'Ch13' },
]

describe('chapterRange', () => {
  it('reads an open end as the single chapter the start names', () => {
    expect(chapterRange('12', '')).toEqual({ low: 12, high: 12 })
  })

  it('reads the ends unordered so a reversed pair still selects what it meant', () => {
    expect(chapterRange('13', '10')).toEqual({ low: 10, high: 13 })
  })

  it('has no span while the start names nothing', () => {
    expect(chapterRange('', '')).toBeNull()
    expect(chapterRange('  ', '')).toBeNull()
  })

  it('has no span when either end names no number', () => {
    // An empty end is the "just this one" case and is read as the start; an end that is present
    // but unparseable is a typo, and silently narrowing to one chapter would export the wrong span.
    expect(chapterRange('12', 'x')).toBeNull()
    expect(chapterRange('x', '')).toBeNull()
  })
})

describe('chaptersInRange', () => {
  it('covers the chapters the span names and skips the deleted ones', () => {
    expect(chaptersInRange(SHELF, 'Demo Title', { low: 10, high: 13 })).toEqual([
      { series: 'Demo Title', chapter: 'Ch10' },
      { series: 'Demo Title', chapter: 'Ch12' },
      { series: 'Demo Title', chapter: 'Ch13' },
    ])
  })

  it('addresses each chapter in its own series directory', () => {
    expect(chaptersInRange(SHELF, 'Other', { low: 12, high: 12 })).toEqual([
      { series: 'Other', chapter: 'Ch12' },
    ])
  })

  it('covers nothing when the span falls in a gap or past the end', () => {
    expect(chaptersInRange(SHELF, 'Demo Title', { low: 11, high: 11 })).toEqual([])
    expect(chaptersInRange(SHELF, 'Demo Title', { low: 14, high: 20 })).toEqual([])
  })
})