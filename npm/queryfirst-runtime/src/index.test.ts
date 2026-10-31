import { describe, expect, it } from 'vitest';
import { QueryfirstCardinalityError, maybe, none, one } from './index';

function fake(n: number): any {
  return { run: async () => Array.from({ length: n }, (_, i) => ({ id: i + 1 })) };
}
const db: any = {};

describe('one', () => {
  it('returns the single row', async () => {
    expect(await one(fake(1), 'getUser').run({}, db)).toEqual({ id: 1 });
  });
  for (const n of [0, 2]) {
    it(`throws for ${n} rows`, async () => {
      const err: any = await one(fake(n), 'getUser').run({}, db).catch((e: any) => e);
      expect(err).toBeInstanceOf(QueryfirstCardinalityError);
      expect(err.queryName).toBe('getUser');
      expect(err.expected).toBe('one');
      expect(err.actual).toBe(n);
      expect(err.message).toBe(`Queryfirst: query "getUser" expected exactly one row, got ${n}`);
    });
  }
});

describe('maybe', () => {
  it('returns null for 0 rows', async () => {
    expect(await maybe(fake(0), 'q').run({}, db)).toBeNull();
  });
  it('returns the row for 1', async () => {
    expect(await maybe(fake(1), 'q').run({}, db)).toEqual({ id: 1 });
  });
  it('throws for 2', async () => {
    const err: any = await maybe(fake(2), 'q').run({}, db).catch((e: any) => e);
    expect(err).toBeInstanceOf(QueryfirstCardinalityError);
    expect(err.expected).toBe('maybe');
    expect(err.actual).toBe(2);
    expect(err.message).toBe('Queryfirst: query "q" expected at most one row, got 2');
  });
});

describe('none', () => {
  it('resolves undefined', async () => {
    expect(await none(fake(3), 'q').run({}, db)).toBeUndefined();
  });
});
