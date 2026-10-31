-- expect: one=notnull s=notnull t=notnull f=notnull d=notnull n=nullable:expression nt=nullable:expression
SELECT 1 AS one, 'x' AS s, true AS t, 1.5 AS f, '2020-01-01'::date AS d, NULL AS n, NULL::text AS nt
