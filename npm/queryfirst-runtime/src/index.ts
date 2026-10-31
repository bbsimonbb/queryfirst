import type { IDatabaseConnection, PreparedQuery } from '@pgtyped/runtime';

export type Cardinality = 'one' | 'maybe' | 'none';

export class QueryfirstCardinalityError extends Error {
  readonly queryName: string;
  readonly expected: Cardinality;
  readonly actual: number;

  constructor(queryName: string, expected: Cardinality, actual: number) {
    const what = expected === 'one' ? 'exactly one row' : 'at most one row';
    super(`Queryfirst: query "${queryName}" expected ${what}, got ${actual}`);
    this.name = 'QueryfirstCardinalityError';
    this.queryName = queryName;
    this.expected = expected;
    this.actual = actual;
    Object.setPrototypeOf(this, QueryfirstCardinalityError.prototype);
  }
}

export function one<P, R>(
  q: PreparedQuery<P, R>,
  queryName: string,
): { run(params: P, db: IDatabaseConnection): Promise<R> } {
  return {
    async run(params, db) {
      const rows = await q.run(params, db);
      if (rows.length !== 1) throw new QueryfirstCardinalityError(queryName, 'one', rows.length);
      return rows[0];
    },
  };
}

export function maybe<P, R>(
  q: PreparedQuery<P, R>,
  queryName: string,
): { run(params: P, db: IDatabaseConnection): Promise<R | null> } {
  return {
    async run(params, db) {
      const rows = await q.run(params, db);
      if (rows.length > 1) throw new QueryfirstCardinalityError(queryName, 'maybe', rows.length);
      return rows.length === 1 ? rows[0] : null;
    },
  };
}

export function none<P, R>(
  q: PreparedQuery<P, R>,
  _queryName: string,
): { run(params: P, db: IDatabaseConnection): Promise<void> } {
  return {
    async run(params, db) {
      await q.run(params, db);
    },
  };
}
